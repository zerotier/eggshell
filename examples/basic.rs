use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), eggshell::Error> {
    let docker = Arc::new(tokio::sync::Mutex::new(
        bollard::Docker::connect_with_unix_defaults().unwrap(),
    ));

    let mut gs = eggshell::EggShell::new(docker.clone()).await?;

    let count = docker
        .lock()
        .await
        .list_containers::<String>(None)
        .await
        .unwrap()
        .len();

    println!(
        "before: {} containers -- starting 10 postgres containers",
        count
    );

    for num in 0..10 {
        gs.launch(
            &format!("test-{}", num),
            bollard::container::Config {
                image: Some("postgres:latest".to_string()),
                env: Some(vec!["POSTGRES_HOST_AUTH_METHOD=trust".to_string()]),
                ..Default::default()
            },
            None,
        )
        .await?;
    }

    let newcount = docker
        .lock()
        .await
        .list_containers::<String>(None)
        .await
        .unwrap()
        .len();

    println!(
        "before: {} containers, after: {} containers -- now dropping",
        count, newcount
    );

    drop(gs);

    let newcount = docker
        .lock()
        .await
        .list_containers::<String>(None)
        .await
        .unwrap()
        .len();

    println!(
        "after dropping: orig: {} containers, after: {} containers",
        count, newcount
    );

    Ok(())
}
