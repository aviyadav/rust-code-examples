// These are just little marker traits for our different states
trait HasHost {}
trait HasPort {}
// The very first state - nothing's set yet, no host, no port
#[derive(Debug)]
struct ConfigBuilder;
impl ConfigBuilder {
    fn new() -> Self {
        ConfigBuilder
    }
    fn set_host(self, host: String) -> ConfigBuilderWithHost {
        println!("Host set: {}", host);
        ConfigBuilderWithHost { host }
    }
}
// Okay, now we're in the state where the host is set
#[derive(Debug)]
struct ConfigBuilderWithHost {
    host: String,
}
impl HasHost for ConfigBuilderWithHost {} // Yep, this struct totally has a host now
impl ConfigBuilderWithHost {
    fn set_port(self, port: u16) -> ConfigBuilderWithHostAndPort {
        println!("Port set: {}", port);
        ConfigBuilderWithHostAndPort {
            host: self.host,
            port,
        }
    }
}
// And finally, the state where both host and port are ready to go
#[derive(Debug)]
struct ConfigBuilderWithHostAndPort {
    host: String,
    port: u16,
}
impl HasHost for ConfigBuilderWithHostAndPort {} // Host? Check!
impl HasPort for ConfigBuilderWithHostAndPort {} // Port? Check!
impl ConfigBuilderWithHostAndPort {
    fn build(self) -> NetworkConfig {
        println!("Building NetworkConfig... ✅");
        NetworkConfig {
            host: self.host,
            port: self.port,
        }
    }
}
#[derive(Debug)]
struct NetworkConfig {
    host: String,
    port: u16,
}
fn main() {
    let config = ConfigBuilder::new()
        .set_host("localhost".to_string())
        .set_port(8080)
        .build();
    println!("Final Network Config: {:?}", config);
    // Get this - this next bit would totally NOT compile!
    // let invalid_config = ConfigBuilder::new()
    //     .set_port(8080) // Error: `ConfigBuilder` has no method named `set_port`
    //     .set_host("127.0.0.1".to_string())
    //     .build();
    // And this one wouldn't compile either!
    // let half_baked_config = ConfigBuilder::new()
    //     .set_host("example.com".to_string());
    // // half_baked_config.build(); // Error: `ConfigBuilderWithHost` has no method named `build`
}
