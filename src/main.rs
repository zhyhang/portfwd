use crate::cmd_line::InputArgs;
use crate::nio::Cluster;
use clap::Parser;

mod cmd_line;
mod nio;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    //args from command line: local,remote addr pairs
    let args_ux: InputArgs = InputArgs::parse();
    let mut cluster = Cluster::new()?;
    cluster.start_with(args_ux)?;
    Ok(())
}
