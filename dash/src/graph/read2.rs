use super::execute::Execute;
use super::filestream::FileStream;
use super::info::Info;
use super::pipe::SharedChannelMap;
use super::rapper::copy_wrapper as copy;
use super::{program, stream, Location, Result};
use failure::bail;
use program::{NodeId, ProgId};
use std::path::PathBuf;
use stream::{
    DashStream, HandleIdentifier, IOType, NetStream, PipeStream, SharedPipeMap, SharedStreamMap,
};
use tracing::error;

/// Node that reads from files and sends the output to the specified outputs.
#[derive(Serialize, Deserialize, PartialEq, Debug, Default)]
pub struct ReadNode {
    /// Id within the program.
    node_id: NodeId,
    /// Id of the program.
    prog_id: ProgId,
    /// Input streams to the read node (note: must be file streams)
    input: FileStream,
    /// Output stream for the read node
    stdout: DashStream,
    /// Execution location of read node.
    location: Location,
}

impl ReadNode {
    pub fn get_stdout_mut(&mut self) -> &mut DashStream {
        &mut self.stdout
    }

    pub fn get_input_location(&self) -> Result<Location> {
        Ok(self.input.get_location())
    }
}

impl Info for ReadNode {
    fn set_id(&mut self, id: NodeId) {
        self.node_id = id;
    }

    fn get_id(&self) -> NodeId {
        self.node_id
    }

    fn get_loc(&self) -> Location {
        self.location.clone()
    }

    fn set_loc(&mut self, loc: Location) {
        self.location = loc;
    }

    fn get_stdin(&self) -> Vec<DashStream> {
        vec![DashStream::File(self.input.clone())]
    }

    fn get_stdout(&self) -> Option<DashStream> {
        Some(self.stdout.clone())
    }

    fn get_stderr(&self) -> Option<DashStream> {
        unimplemented!()
    }

    fn get_stdin_len(&self) -> usize {
        1
    }

    fn get_stdout_len(&self) -> usize {
        1
    }

    fn get_stderr_len(&self) -> usize {
        0
    }

    fn add_stdin(&mut self, stream: DashStream) {
        match stream {
            DashStream::File(fs) => {
                self.input = fs;
            }
            _ => {
                panic!(
                    "Setting stdin on filestream to be a non-file stream: {:?}",
                    stream
                );
            }
        }
    }

    fn set_stdout(&mut self, stream: DashStream) {
        self.stdout = stream;
    }

    fn set_stderr(&mut self, _stream: DashStream) {
        unimplemented!()
    }

    fn get_dot_label(&self) -> Result<String> {
        Ok(format!(
            "{}: {:?}\nloc: {:?}",
            self.node_id, self.input, self.location
        ))
    }

    fn resolve_args(&mut self, parent_dir: PathBuf) -> Result<()> {
        // resolve the location of the input filestream
        self.input.prepend_directory(parent_dir.as_path());
        Ok(())
    }

    /// Modify the pipe to be a netstream.
    fn replace_pipe_with_net(
        &mut self,
        pipe: PipeStream,
        net: NetStream,
        iotype: IOType,
    ) -> Result<()> {
        match iotype {
            IOType::Stdout => match &self.stdout {
                DashStream::Pipe(ps) => {
                    if *ps == pipe {
                        self.stdout = DashStream::Tcp(net);
                        Ok(())
                    } else {
                        error!("In replace_pipe_with_net, pipe {:?} doesn't exist to replace with net {:?}", pipe, net);
                        bail!("Pipe doesn't exist in replace_pipe_with_net");
                    }
                }
                _ => {
                    error!("In replace_pipe_with_net, pipe {:?} doesn't exist to replace with net {:?}", pipe, net);
                    bail!("Pipe doesn't exist in replace_pipe_with_net");
                }
            },
            _ => Ok(()),
        }
    }
}

impl Execute for ReadNode {
    fn spawn(
        &mut self,
        _pipes: SharedPipeMap,
        _network_connections: SharedStreamMap,
        _channels: SharedChannelMap,
        _tmp_folder: PathBuf,
    ) -> Result<()> {
        Ok(())
    }
    fn redirect(
        &mut self,
        mut pipes: SharedPipeMap,
        mut network_connections: SharedStreamMap,
        _channels: SharedChannelMap,
        _tmp_folder: PathBuf,
    ) -> Result<()> {
        let mut file_handle = self.input.open()?;
        match &self.stdout {
            DashStream::Tcp(netstream) => {
                let mut tcpstream = network_connections.remove(&netstream)?;
                // hopefully this will immediately block until the next process is ready
                copy(&mut file_handle, &mut tcpstream)?;
            }
            // TODO: technically if multiple nodes and writing to one node -> then the aggregate
            // node should decide when to pull into the pipe
            // But realistically: when are you going to have multiple non write nodes into the same
            // node?
            DashStream::Pipe(pipestream) => {
                let handle_identifier =
                    HandleIdentifier::new(self.prog_id, self.node_id, pipestream.get_output_type());
                let mut input_handle = pipes.remove(&handle_identifier)?;
                copy(&mut file_handle, &mut input_handle)?;
            }
            _ => {
                error!(
                    "Read node should not send output to a file, stdout, or stderr handle: {:?}",
                    self.stdout
                );
                bail!(
                    "Read node should not send output to a file, stdout, or stderr handle: {:?}",
                    self.stdout
                );
            }
        }
        Ok(())
    }
}
