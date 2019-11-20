use super::rapper::{resolve_file_streams, stream_initiate_filter, Rapper};
use super::{program, stream, Location, Result};
use failure::bail;
use program::{NodeId, ProgId};
use std::fs::OpenOptions;
use std::io::{copy, stderr, stdout};
use stream::{DashStream, HandleIdentifier, IOType, NetStream, SharedPipeMap, SharedStreamMap};
/// Node that writes stdin to a specified file.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct WriteNode {
    /// Id within the program.
    node_id: NodeId,
    /// Id of the program.
    prog_id: ProgId,
    /// Input streams to write node.
    stdin: Vec<DashStream>,
    /// Output streams (note: must be file streams).
    output: Vec<DashStream>,
    /// Execution location of the read node.
    location: Location,
}

impl Rapper for WriteNode {
    fn get_outward_streams(&self, iotype: IOType, is_server: bool) -> Vec<NetStream> {
        // Only look at stdin streams; output MUST be a file on the same machine.
        let streams: Vec<DashStream> = match iotype {
            IOType::Stdin => self
                .stdin
                .clone()
                .iter()
                .filter(|&s| stream_initiate_filter(s.clone(), self.node_id, is_server))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        streams
            .iter()
            .map(|s| {
                let netstream_result: Option<NetStream> = s.clone().into();
                netstream_result.unwrap()
            })
            .collect()
    }
    fn get_stdin(&self) -> Vec<DashStream> {
        self.stdin.clone()
    }

    fn get_stdout(&self) -> Vec<DashStream> {
        self.output.clone()
    }

    fn get_stderr(&self) -> Vec<DashStream> {
        unimplemented!();
    }

    fn add_stdin(&mut self, stream: DashStream) -> Result<()> {
        self.stdin.push(stream);
        Ok(())
    }
    fn add_stdout(&mut self, stream: DashStream) -> Result<()> {
        match stream {
            DashStream::File(fs) => {
                self.output.push(DashStream::File(fs));
            }
            _ => bail!("Adding stdout to write node that is not a file stream."),
        }
        Ok(())
    }

    fn add_stderr(&mut self, _stream: DashStream) -> Result<()> {
        bail!("No stderr for write node");
    }

    fn run_redirection(
        &mut self,
        mut pipes: SharedPipeMap,
        mut network_connections: SharedStreamMap,
    ) -> Result<()> {
        for output_stream in self.output.iter() {
            for stream in self.stdin.iter() {
                match stream {
                    DashStream::Tcp(netstream) => {
                        let mut tcpstream = network_connections.remove(&netstream)?;
                        match output_stream {
                            DashStream::File(filestream) => {
                                let mut file_handle = OpenOptions::new()
                                    .write(true)
                                    .create(true)
                                    .open(filestream.get_name())?;
                                copy(&mut tcpstream, &mut file_handle)?;
                            }
                            DashStream::Stdout => {
                                copy(&mut tcpstream, &mut stdout())?;
                            }
                            DashStream::Stderr => {
                                copy(&mut tcpstream, &mut stderr())?;
                            }
                            _ => {
                                bail!("Output stream is not of type file, stdout or stderr handle: {:?}", output_stream);
                            }
                        }
                    }
                    DashStream::Pipe(pipestream) => {
                        let handle_identifier = HandleIdentifier::new(
                            self.prog_id,
                            self.node_id,
                            pipestream.get_output_type(),
                        );
                        let mut output_handle = pipes.remove(&handle_identifier)?;

                        match output_stream {
                            DashStream::File(filestream) => {
                                let mut file_handle = OpenOptions::new()
                                    .write(true)
                                    .create(true)
                                    .open(filestream.get_name())?;
                                copy(&mut output_handle, &mut file_handle)?;
                            }
                            DashStream::Stdout => {
                                copy(&mut output_handle, &mut stdout())?;
                            }
                            DashStream::Stderr => {
                                copy(&mut output_handle, &mut stderr())?;
                            }
                            _ => {
                                bail!("Output stream is not of type file, stdout or stderr handle: {:?}", output_stream);
                            }
                        }
                    }
                    _ => {
                        bail!("Write node should not see input from a file, stdout, or stderr handle: {:?}", stream);
                    }
                }
            }
        }
        Ok(())
    }

    fn execute(
        &mut self,
        _pipes: SharedPipeMap,
        _network_connections: SharedStreamMap,
    ) -> Result<()> {
        // Noop: a write node just writes the output of streams into files
        // Nothing needs to be spawned beforehand.
        Ok(())
    }

    fn get_loc(&self) -> Location {
        self.location.clone()
    }

    fn resolve_args(&mut self, parent_dir: &str) -> Result<()> {
        resolve_file_streams(&mut self.stdin, parent_dir)?;
        resolve_file_streams(&mut self.output, parent_dir)?;
        Ok(())
    }
}
