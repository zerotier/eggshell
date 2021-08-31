//!
//! EggShell automatically cleans up all the containers it creates when the ship is destroyed. It
//! uses the [bollard](https://crates.io/crates/bollard) docker toolkit, and
//! [tokio](https://crates.io/crates/tokio) internally to manage the containers.  To utilize,
//! simply create containers through it and wait for EggShell to go away. It will automatically
//! trigger its `Drop` implementation which will destroy the containers.
//!
//! EggShell requires a multi-threaded tokio async runtime to function. Please create one in your
//! environment that tokio can "find" with the tokio::runtime::Handle::current() call.
//!
//! If you want to handle signals in your tests, call supervise_signals().
//!

use core::panic;
use std::{future::Future, sync::Arc};

use bollard::{
    container::{CreateContainerOptions, RemoveContainerOptions, StartContainerOptions},
    Docker,
};
use lazy_static::lazy_static;
use thiserror::Error;
use tokio::{runtime::Handle, signal::unix::SignalKind, sync::Mutex};

/// EggShell is basically a container for containers. When provided a docker client, it funnels
/// all container create operations through it, allowing it to track what needs to be cleaned up.
/// At drop() time (or when teardown() is called), these containers are cleaned up.
#[derive(Debug, Clone)]
pub struct EggShell {
    docker: Arc<Mutex<Docker>>,
    containers: Arc<Mutex<Vec<String>>>,
    debug: bool,
}

/// Error is a top-level error enum for EggShell.
#[derive(Debug, Error)]
pub enum Error {
    #[error("error talking to docker: {0}")]
    Docker(DockerError),
    #[error("unknown error: {0}")]
    Generic(String),
}

/// DockerError covers only errors that are from docker specifically.
#[derive(Debug, Error)]
pub enum DockerError {
    #[error("could not ping docker")]
    Ping,
    #[error("could not create container: {0}")]
    CreateContainer(String),
    #[error("could not start container: {0}")]
    StartContainer(String),
    #[error("could not delete container: {0}")]
    DeleteContainer(String),
}

lazy_static! {
    static ref EGGSHELLS: Arc<Mutex<Vec<EggShell>>> = Arc::new(Mutex::new(Vec::new()));
    static ref SUPERVISOR_RUNNING: Arc<Mutex<Option<()>>> = Arc::new(Mutex::new(None));
}

/// Please call this function in your tests with tokio::spawn() to handle signals being sent to
/// your tests. A wait function can be supplied to perform any final settling before terminating
/// the test program.
///
/// In the event multiple signal handlers are spawned, N+1 handlers will simply run the wait hook
/// and return. Only the first execution is responsible for reaping all eggshells.
///
/// ```
/// use eggshell::supervise_signals;
/// use std::time::Duration;
/// use tokio::{runtime::Handle, time::sleep};
///
/// fn signal_handler() {
///     let handle = Handle::current();
///     handle.spawn(supervise_signals(async { sleep(Duration::new(1, 0)).await; }));
/// }
/// ```
pub async fn supervise_signals<F>(wait_hook: F)
where
    F: Future<Output = ()>,
{
    let mut supervisor = SUPERVISOR_RUNNING.lock().await;

    if !supervisor.is_some() {
        supervisor.replace(());
        drop(supervisor)
    } else {
        wait_hook.await;
        return;
    }

    let mut interrupt = tokio::signal::unix::signal(SignalKind::interrupt()).unwrap();
    let mut terminate = tokio::signal::unix::signal(SignalKind::terminate()).unwrap();

    tokio::select! {
    _ = interrupt.recv() => {},
    _ = terminate.recv() => {},
    };

    eprintln!("eggshell: signal received, tearing down containers");

    let mut lock = EGGSHELLS.lock().await.clone();

    let mut i = 0;
    let mut prune_indexes = Vec::new();

    for shell in lock.iter_mut() {
        if let Err(e) = shell.teardown().await {
            eprintln!("error during teardown: {:?}", e);
        } else {
            prune_indexes.push(i)
        }
        i += 1;
    }

    for idx in prune_indexes {
        lock.remove(idx);
    }

    wait_hook.await;

    if lock.len() == 0 {
        std::process::abort()
    }
}

impl EggShell {
    /// Construct a new EggShell. When dropped or when teardown() is called, all containers
    /// launched through it will be reaped.
    pub async fn new(docker: Arc<Mutex<Docker>>) -> Result<Self, Error> {
        match docker.lock().await.ping().await {
            Ok(_) => {}
            Err(_) => return Err(Error::Docker(DockerError::Ping)),
        }

        let this = Self {
            docker,
            containers: Arc::new(Mutex::new(Vec::new())),
            debug: false,
        };

        EGGSHELLS.lock().await.push(this.clone());
        Ok(this)
    }

    /// set_debug turns off the teardown functionality for this EggShell, allowing you to debug
    /// its behavior. This disables the teardown() call which drop() relies on, so you must be
    /// ready to clean up your own containers before enabling this.
    pub fn set_debug(&mut self, debug: bool) {
        self.debug = debug;
    }

    /// launch is the meat of EggShell and is how most of the work gets done. When provided with a
    /// name and container options, it will create and start that container, adding it to a
    /// registry of containers it is tracking. When drop() happens on the EggShell this container
    /// will be removed.
    pub async fn launch(
        &mut self,
        name: &str,
        container: bollard::container::Config<String>,
        start_options: Option<StartContainerOptions<String>>,
    ) -> Result<(), Error> {
        self.containers.lock().await.push(name.to_string());

        match self
            .docker
            .lock()
            .await
            .create_container(Some(CreateContainerOptions { name }), container)
            .await
        {
            Ok(s) => s,
            Err(_) => {
                return Err(Error::Docker(DockerError::CreateContainer(
                    name.to_string(),
                )))
            }
        };

        match self
            .docker
            .lock()
            .await
            .start_container(name.clone(), start_options)
            .await
        {
            Ok(_) => {}
            Err(_) => return Err(Error::Docker(DockerError::StartContainer(name.to_string()))),
        };

        Ok(())
    }

    /// teardown is what reaps the launch()'d containers. It is called as a part of drop() as well.
    pub async fn teardown(&self) -> Result<(), Error> {
        if !self.debug {
            let mut error = None;
            let mut containers = self.containers.lock().await;

            while containers.len() > 0 {
                let len = containers.len();
                let container = &containers[len - 1];

                match self
                    .docker
                    .lock()
                    .await
                    .remove_container(
                        container,
                        Some(RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        }),
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        if EGGSHELLS.lock().await.len() > 1 {
                            error.replace(Err(Error::Docker(DockerError::DeleteContainer(
                                e.to_string(),
                            ))));
                        }
                    }
                }

                containers.remove(len - 1);
            }

            if error.is_some() {
                return error.unwrap();
            }
        }

        Ok(())
    }
}

impl Drop for EggShell {
    /// Reaps all containers registered with launch().
    fn drop(&mut self) {
        tokio::task::block_in_place(move || Handle::current().block_on(self.teardown()).unwrap());
    }
}

mod tests {

    #[tokio::test(flavor = "multi_thread")]
    async fn basic() {
        use crate::supervise_signals;
        use crate::EggShell;
        use bollard::container::Config;
        use bollard::Docker;
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::sync::Mutex;
        use tokio::time::sleep;

        // you should be able to ^C this test fine without a process hanging past the time all the
        // docker containers are cleaned up.
        tokio::spawn(supervise_signals(async {
            sleep(Duration::new(1, 0)).await
        }));
        tokio::spawn(supervise_signals(async { () }));
        tokio::spawn(supervise_signals(async { () }));
        tokio::spawn(supervise_signals(async { () }));
        tokio::spawn(supervise_signals(async { () }));
        tokio::spawn(supervise_signals(async { () }));
        tokio::spawn(supervise_signals(async { () }));
        tokio::spawn(supervise_signals(async { () }));

        let units = 20;

        let docker = Arc::new(Mutex::new(Docker::connect_with_unix_defaults().unwrap()));

        let res = EggShell::new(docker.clone()).await;
        assert!(res.is_ok());

        let count = docker
            .lock()
            .await
            .list_containers::<String>(None)
            .await
            .unwrap()
            .len();

        let mut gs = res.unwrap();

        for num in 0..units {
            let res = gs
                .launch(
                    &format!("test-{}", num),
                    Config {
                        image: Some("postgres:latest".to_string()),
                        env: Some(vec!["POSTGRES_HOST_AUTH_METHOD=trust".to_string()]),
                        ..Default::default()
                    },
                    None,
                )
                .await;

            assert!(res.is_ok())
        }

        let newcount = docker
            .lock()
            .await
            .list_containers::<String>(None)
            .await
            .unwrap()
            .len();

        assert!(newcount == count + units);

        drop(gs);

        let newcount = docker
            .lock()
            .await
            .list_containers::<String>(None)
            .await
            .unwrap()
            .len();

        assert!(newcount == count);
    }
}
