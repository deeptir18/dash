extern crate shell;
extern crate structopt_derive;

use dash::util::Result;
use failure::bail;
use shell::interpreter::examples;
use shell::shellparser::shellparser;
use std::path::Path;
use std::process::Command;
use structopt::StructOpt;
use tracing::{error, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "visualization_binary",
    help = "Binary to help visualize commands"
)]

struct Opt {
    #[structopt(
        short = "of",
        long = "output_folder",
        help = "Place to write dot files and binaries to."
    )]
    output_folder: String,
    #[structopt(short = "dot", long = "dot_binary", help = "Location of dot binary")]
    dot_binary: String,
}
enum VizType {
    Shell,
    Dash,
}
fn main() {
    let opt = Opt::from_args();
    let output_folder = opt.output_folder;
    let dot_binary = opt.dot_binary;
    // global tracing settings
    let subscriber = FmtSubscriber::builder()
        // all spans/events with a level higher than TRACE (e.g, debug, info, warn, etc.)
        // will be written to stdout.
        .with_max_level(Level::TRACE)
        // completes the builder.
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("setting defualt subscriber failed");
    run_viz(
        &dot_binary,
        "cat /d/c/b/1.INFO | grep '[RAY]' | head -n1 | cut -c 7- > /d/c/b/rays.csv",
        "rt_cmd1",
        &output_folder,
        VizType::Shell,
    );
    run_viz(
        &dot_binary,
        "cat /d/c/b/1.INFO | grep '[RAY]' | head -n1 | cut -c 7- > /d/c/b/rays.csv",
        "rt_cmd1",
        &output_folder,
        VizType::Dash,
    );
    run_viz(
        &dot_binary,
        "cat /d/c/b/2.INFO /d/c/b/3.INFO /d/c/b/4.INFO | grep -v pathID | cut -c 7- >> rays.csv",
        "rt_cmd2",
        &output_folder,
        VizType::Dash,
    );
    run_viz(
        &dot_binary,
        "cat /d/c/b/FILENAME |  zannotate -routing -routing-mrt-file=/d/c/b/mrt_file -input-file-type=json > /d/c/b/annotated",
        "portscan_preprocess",
        &output_folder,
        VizType::Dash,
        );
    run_viz(
        &dot_binary,
    "pr -mts, <( cat /d/c/b/annotated | jq \".ip\" | tr -d '\"' ) <( cat /d/c/b/annotated | jq -c \".zannotate.routing.asn\" ) | awk -F',' '{ a[$2]++; } END { for (n in a) print n \",\" a[n] } ' | sort -k2 -n -t',' -r > b/as_popularity",
        "port_scan_cmd",
        &output_folder,
        VizType::Dash,
    );

    run_viz(
        &dot_binary,
        "cat /d/c/foo /b/a/foo /e/d/foo /f/e/foo | grep 'bar' > /local/local.txt",
        "distributed_cat",
        &output_folder,
        VizType::Shell,
    );
    run_viz(
        &dot_binary,
        "cat /d/c/foo /b/a/foo /e/d/foo /f/e/foo | grep 'bar' > /local/local.txt",
        "distributed_cat",
        &output_folder,
        VizType::Dash,
    );

    run_viz(
        &dot_binary,
        "git clone https://github.com/deeptir18/dash /d/c/dash",
        "git_clone",
        &output_folder,
        VizType::Dash,
    );

    run_viz(
        &dot_binary,
        "git commit -m \"fake commit message\"",
        "git_commit",
        &output_folder,
        VizType::Dash,
    );

    run_viz(
        &dot_binary,
        "cat *.toml",
        "cat_wildcard",
        &output_folder,
        VizType::Dash,
    );

    run_viz(
        &dot_binary,
        "comm -12 /f/e/file1.txt /b/a/file1.txt > /local/local.txt",
        "comm_basic",
        &output_folder,
        VizType::Dash,
    );
}

fn run_viz(dot_binary: &str, cmd: &str, name: &str, output_folder: &str, viztype: VizType) {
    match viztype {
        VizType::Shell => match visualize_shell_graph(dot_binary, cmd, name, output_folder) {
            Ok(_) => {}
            Err(e) => {
                error!("Failed to visualize shell graph: {:?}", e);
            }
        },
        VizType::Dash => match visualize_dash_graph(dot_binary, cmd, name, output_folder) {
            Ok(_) => {}
            Err(e) => {
                error!("Failed to visualize dash graph: {:?}", e);
            }
        },
    }
}

fn visualize_shell_graph(dot_binary: &str, command: &str, name: &str, folder: &str) -> Result<()> {
    let file = Path::new(folder);
    let dot_path = file.join(format!("{}_shell_viz.dot", name));
    let graph_path = file.join(format!("{}_shell_viz.pdf", name));
    let dot_path_str = match dot_path.to_str() {
        Some(s) => s,
        None => bail!("Could not turn path: {:?}, shell_viz.dot", file),
    };
    let graph_path_str = match graph_path.to_str() {
        Some(s) => s,
        None => bail!("Could not turn path: {:?}, shell_viz.pdf", file),
    };

    // generate shell graph
    let shellsplit = shellparser::ShellSplit::new(command)?;
    let shellgraph = shellsplit.convert_into_shell_graph()?;
    shellgraph.write_dot(dot_path_str)?;
    // invoke graphviz
    invoke_graph_viz(dot_binary, dot_path_str, graph_path_str)?;
    Ok(())
}

fn visualize_dash_graph(dot_binary: &str, command: &str, name: &str, folder: &str) -> Result<()> {
    tracing::info!("Visualizing cmd {:?} with invocation {:?}", name, command);
    let file = Path::new(folder);
    let dot_path = file.join(format!("{}_dash_viz.dot", name));
    let graph_path = file.join(format!("{}_dash_viz.pdf", name));
    let dot_path_str = match dot_path.to_str() {
        Some(s) => s,
        None => bail!("Could not turn path: {:?}, dash_viz.dot", file),
    };
    let graph_path_str = match graph_path.to_str() {
        Some(s) => s,
        None => bail!("Could not turn path: {:?}, dash_viz.pdf", file),
    };

    let mut interpreter = examples::get_test_interpreter();
    interpreter.set_splitting_factor(2);
    let program = match interpreter.parse_command_line(command)? {
        Some(prog) => prog,
        None => {
            bail!("Parsing didn't return program");
        }
    };
    // invoke graphviz
    program.write_dot(dot_path_str)?;
    invoke_graph_viz(dot_binary, dot_path_str, graph_path_str)?;
    Ok(())
}

fn invoke_graph_viz(binary_path: &str, dot_path: &str, graph_path: &str) -> Result<()> {
    // dot basic.dot -Tpdf -o basic.pdf
    let _output = Command::new(binary_path)
        .arg(dot_path)
        .arg("-Tpdf")
        .arg("-o")
        .arg(graph_path)
        .output()
        .expect("Failed to run dot command");
    Ok(())
}
