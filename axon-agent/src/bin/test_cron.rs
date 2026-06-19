use std::str::FromStr;
fn main() {
    let cron = "0 0 */5 * * *";
    match cron::Schedule::from_str(cron) {
        Ok(_) => println!("OK"),
        Err(e) => println!("ERROR: {}", e),
    }
}
