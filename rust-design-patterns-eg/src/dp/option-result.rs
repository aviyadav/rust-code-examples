// Using Option for something that might not exist
fn get_username_by_id(id: u32) -> Option<String> {
    // In a real app, this would probably hit a database or a cache, you know?
    match id {
        1 => Some("Alice".to_string()),
        2 => Some("Bob".to_string()),
        _ => None, // Well, no user found for that ID
    }
}
// Using Result for an operation that might, like, fail
fn parse_port(port_str: &str) -> Result<u16, String> {
    port_str
        .parse::<u16>()
        .map_err(|e| format!("Invalid port format: {}", e)) // Turn the parse error into our String error
        .and_then(|port| {
            if port > 1023 {
                Ok(port)
            } else {
                Err(format!("Port {} is outside valid range (1024-65535)", port))
            }
        })
}
fn main() {
    // Option example time!
    match get_username_by_id(1) {
        Some(name) => println!("Found user: {}", name),
        None => println!("User not found!"),
    }
    match get_username_by_id(3) {
        Some(name) => println!("Found user: {}", name),
        None => println!("User not found!"),
    }
    println!("---");
    // Result example next!
    match parse_port("8080") {
        Ok(port) => println!("Successfully parsed port: {}", port),
        Err(e) => eprintln!("Error parsing port: {}", e),
    }
    match parse_port("invalid") {
        Ok(port) => println!("Successfully parsed port: {}", port),
        Err(e) => eprintln!("Error parsing port: {}", e),
    }
    match parse_port("80") {
        Ok(port) => println!("Successfully parsed port: {}", port),
        Err(e) => eprintln!("Error parsing port: {}", e),
    }
    // Just a quick peek at the `?` operator (if we were in a function that returns a Result, that is)
    // fn process_config(input: &str) -> Result<u16, String> {
    //     let port = parse_port(input)?; // This would just pass the error right along if parsing fails
    //     Ok(port * 2)
    // }
}
