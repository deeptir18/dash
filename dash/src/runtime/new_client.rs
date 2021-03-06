use super::graph::{program, stream, Location};
use super::runtime_util::Addr;
use super::serialize::{read_msg_and_type, rpc, write_msg_and_type};
use super::Result;
use bincode::{deserialize, serialize};
use failure::bail;
use std::collections::HashMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::str;
use std::thread;
use stream::{NetStream, SharedStreamMap};
use thread::JoinHandle;
use tracing::{debug, error, info};

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct ShellClient {
    /// Server port
    port: String,
    /// Current directory; used to resolve file paths locally in case any commands change.
    pwd: PathBuf,
    /// Tmp file. File client can use for temporarily storing output of files.
    tmp: String,
}

impl ShellClient {
    pub fn new(server_port: &str, pwd: PathBuf, tmp: &str) -> Result<Self> {
        Ok(ShellClient {
            port: server_port.to_string(),
            pwd: pwd,
            tmp: tmp.to_string(),
        })
    }

    pub fn set_pwd(&mut self, pwd: PathBuf) {
        self.pwd = pwd;
    }

    /// Runs the setup portion of the command.
    fn run_setup(
        &self,
        program_map: &mut HashMap<Location, program::Program>,
        shared_map: &mut SharedStreamMap,
    ) -> Result<()> {
        let mut setup_threads: Vec<JoinHandle<Result<()>>> = Vec::new();
        // 1: wait for all the servers to setup their connections
        for (loc, prog) in program_map.iter_mut() {
            // get all the connections (e.g., stream identifiers) this part of the graph should
            // initiate
            let outward_connections = prog.get_outward_streams(loc.clone());
            for netstream in outward_connections.iter() {
                let map_clone = shared_map.clone();
                let prog_id = prog.get_id();
                let netstream_clone = netstream.clone();
                let port = self.port.clone();
                setup_threads.push(match loc.clone() {
                    Location::Client => thread::spawn(move || {
                        run_stream_setup(netstream_clone, port, map_clone, prog_id)
                    }),
                    Location::Server(_ip) => thread::spawn(move || {
                        run_stream_setup(netstream_clone, port, map_clone, prog_id)
                    }),
                });
            }
        }

        // When all these setup threads are joined,
        // safe to start executing the program.
        for handle in setup_threads {
            match handle.join() {
                Ok(res) => match res {
                    Ok(_) => {}
                    Err(e) => {
                        bail!("One SETUP thread had an error: {:?}", e);
                    }
                },
                Err(e) => {
                    bail!("Error in joining the setup threadi: {:?}", e);
                }
            }
        }

        Ok(())
    }

    fn send_program(
        &self,
        program_map: &mut HashMap<Location, program::Program>,
        shared_map: &mut SharedStreamMap,
    ) -> Result<()> {
        let mut execution_threads: Vec<JoinHandle<Result<()>>> = Vec::new();
        for (loc, prog) in program_map.iter_mut() {
            let location = loc.clone();
            let program = prog.clone();
            let shared_map_copy = shared_map.clone();
            let port = self.port.clone();
            let tmp_folder = self.tmp.clone();
            execution_threads.push(thread::spawn(move || {
                let ret = execute_subprogram(
                    location.clone(),
                    program,
                    shared_map_copy,
                    port,
                    tmp_folder,
                );
                debug!("One of threads joined: {:?}", location);
                ret
            }));
        }

        for handle in execution_threads {
            match handle.join() {
                Ok(res) => match res {
                    Ok(_) => {}
                    Err(e) => {
                        bail!("One Execution thread had an error: {:?}", e);
                    }
                },
                Err(e) => {
                    bail!("Error in joining the execution thread: {:?}", e);
                }
            }
        }
        Ok(())
    }

    /// Executes the given program by offloading the relevant nodes to the correct machines.
    pub fn run_command(&self, program: program::Program) -> Result<()> {
        // split the program into portions that each node needs execute
        let mut program_map = match program.split_by_machine() {
            Ok(m) => m,
            Err(e) => {
                bail!("Could not split given program: {:?}", e);
            }
        };

        // client needs a shared stream map for handling copying standard in to nodes,
        // for the portions of the graph *it needs to execute*
        let mut shared_map = SharedStreamMap::new();
        self.run_setup(&mut program_map, &mut shared_map)?;
        // now try to execute each portion of the program:
        self.send_program(&mut program_map, &mut shared_map)?;
        Ok(())
    }

    /// Asks servers to stat given files.
    pub fn stat_files(
        &self,
        requests: HashMap<Location, Vec<PathBuf>>,
    ) -> Result<HashMap<Location, rpc::SizeRequest>> {
        let mut results: HashMap<Location, rpc::SizeRequest> = HashMap::default();
        // for each location, spawn a thread that handles requesting the sizes for those paths
        let mut size_threads: Vec<JoinHandle<Result<(Location, rpc::SizeRequest)>>> = Vec::new();

        for (location, paths) in requests.iter() {
            let size_request = rpc::SizeRequest {
                files: paths.clone(),
                sizes: vec![],
                failed: false,
            };
            let loc_clone = location.clone();
            let port_clone = self.port.clone();
            size_threads.push(thread::spawn(move || {
                tracing::debug!("size request thread to {:?}", loc_clone);
                let ip = match loc_clone.clone() {
                    Location::Client => {
                        bail!("Should not be sending size req to client");
                    }
                    Location::Server(ip) => ip,
                };
                let addr = Addr::new(&ip, &port_clone).get_addr();
                let mut stream = TcpStream::connect(addr)?;
                let message = serialize(&size_request)?;
                write_msg_and_type(message.to_vec(), rpc::MessageType::SizeRequest, &mut stream)?;
                let (_, next_msg) = read_msg_and_type(&mut stream)?;
                let msg: rpc::SizeRequest = deserialize(&next_msg[..])?;
                if msg.failed {
                    bail!(
                        "Size req for {:?}, paths {:?} failed",
                        loc_clone,
                        size_request
                    );
                }
                Ok((loc_clone, msg))
            }));
        }

        for handle in size_threads {
            match handle.join() {
                Ok(val) => match val {
                    Ok((loc, size_req)) => {
                        results.insert(loc, size_req);
                    }
                    Err(e) => {
                        bail!("Thread failed to join size request with err {:?}", e);
                    }
                },
                Err(e) => {
                    bail!("Querying for size thread failed to join with err {:?}", e);
                }
            }
        }

        Ok(results)
    }
}

/// Makes open stream requests
/// from_loc: server to initiate the pipe message
/// to_loc: server to receive the pipe message
/// stream_identifier: Information about the stream metadata
/// port: Port on which client sends messages to the servers
/// map: SharedStreamMap - client will need to insert the resulting streams into a map in order to
/// later use them when executing the client's portion of the program
fn run_stream_setup(
    netstream: NetStream,
    port: String,
    mut map: SharedStreamMap,
    prog_id: program::ProgId,
) -> Result<()> {
    match netstream.get_sending_side() {
        Location::Client => {
            let addr = match netstream.get_receiving_side() {
                Location::Server(ip) => Addr::new(&ip, &port).get_addr(),
                Location::Client => {
                    bail!("From loc and to loc are client");
                }
            };
            let mut stream = TcpStream::connect(addr)?;
            // send a stream connection message
            // TODO:edo we need to convert the stream_identifier in anyway?
            let netstream_info: rpc::NetworkStreamInfo = rpc::NetworkStreamInfo {
                loc: Location::Client,
                port: port.clone(),
                prog_id: prog_id,
                netstream: netstream.clone(),
            };
            let msg = serialize(&netstream_info)?;
            write_msg_and_type(msg.to_vec(), rpc::MessageType::Pipe, &mut stream)?;

            // wait for the success:
            let (_, response_buf) = read_msg_and_type(&mut stream)?;
            let response: rpc::ClientReturnCode = deserialize(&response_buf[..])?;
            match response {
                rpc::ClientReturnCode::Success => {}
                rpc::ClientReturnCode::Failure => {
                    bail!("Failed to setup stream to send on server");
                }
            }

            // the client thread that runs the programs needs access to these streams as well
            // need to set the reading side of the stream to be nonblocking.
            // TODO: would need to do this for all the streams
            stream.set_nonblocking(true)?;
            let clone = stream.try_clone()?;
            map.insert(netstream.clone(), clone)?;
            drop(stream);
            Ok(())
        }
        Location::Server(ip) => {
            debug!("setup thread to {:?}", ip);
            let addr = Addr::new(&ip, &port).get_addr();
            let mut stream = TcpStream::connect(addr)?;
            let info = rpc::NetworkStreamInfo {
                loc: netstream.get_receiving_side().clone(),
                port: port.clone(),
                prog_id: prog_id,
                netstream: netstream.clone(),
            };
            let message = serialize(&info)?;
            write_msg_and_type(
                message.to_vec(),
                rpc::MessageType::SetupStreams,
                &mut stream,
            )?;
            let (_, next_msg) = read_msg_and_type(&mut stream)?;
            let msg = deserialize(&next_msg[..])?;
            match msg {
                rpc::ClientReturnCode::Success => Ok(()),
                rpc::ClientReturnCode::Failure => {
                    bail!("Server returned failure to open new stream to another server")
                }
            }
        }
    }
}

/// Executes a subprogram by either:
/// executing the program on the client,
/// or executing the program on the server and waiting
/// for the results.
/// loc: Location -> location of this subprogram.
/// program: Program -> subprogram to be executed.
/// shared_map: SharedStreamMap: handle for map with client's subprogram TCP streams.
/// port: String -> port that server is listening to
pub fn execute_subprogram(
    loc: Location,
    mut prog: program::Program,
    shared_stream_map: SharedStreamMap,
    port: String,
    tmp_folder: String,
) -> Result<()> {
    tracing::warn!("Sending program {:?} to loc {:?} for execution", prog, loc);
    match loc {
        Location::Client => {
            // execute the subprogram
            debug!("executing following subprogram locally: {:?}", prog);
            prog.resolve_args("")?; // noop for client
            match prog.execute(shared_stream_map, tmp_folder) {
                Ok(_) => {
                    info!("Client executed successfully!");
                    return Ok(());
                }
                Err(e) => {
                    error!("Client failed with error e: {:?}", e);
                    return Err(e);
                }
            }
        }
        Location::Server(ip) => {
            // send a request to the server to execute this subprogram
            let addr = Addr::new(&ip, &port).get_addr();
            let mut stream = TcpStream::connect(addr)?;
            let message = serialize(&prog)?;
            write_msg_and_type(
                message.to_vec(),
                rpc::MessageType::ProgramExecution,
                &mut stream,
            )?;
            stream.set_nonblocking(false)?;
            let (_, next_msg) = read_msg_and_type(&mut stream)?;
            let msg = deserialize(&next_msg[..])?;
            match msg {
                rpc::ClientReturnCode::Success => {
                    info!("Server returned success for program execution: {:?}", ip);
                    return Ok(());
                }
                rpc::ClientReturnCode::Failure => {
                    error!("Server failed to execute: {:?}", ip);
                    bail!("Server returned failure to execute program.")
                }
            }
        }
    }
}
