# EggShell (for rust): automatically destroy containers you create

EggShell automatically cleans up all the containers it creates when the struct is dropped. It uses the [bollard](https://crates.io/crates/bollard) docker toolkit, and [tokio](https://crates.io/crates/tokio) internally to manage the containers.

To utilize, simply create containers through it and wait for EggShell to go away. It will automatically trigger its `Drop` implementation which will destroy the containers.

Example that shows off counting (from the `examples/basic.rs` source):

```rust
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
```

## Author

Erik Hollensbe <erik.hollensbe@zerotier.com>

## License

BSD 3-Clause
