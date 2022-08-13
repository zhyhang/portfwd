extern crate core;

use std::env;

use crate::nio::Cluster;

mod nio;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    //args from command line: local,remote addr pairs
    let args: Vec<String> = env::args().collect();
    let mut cluster = Cluster::new()?;
    cluster.start_with(&args[1..]);
    Ok(())
}


