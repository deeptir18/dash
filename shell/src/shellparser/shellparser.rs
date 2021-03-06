extern crate dash;
extern crate itertools;
extern crate shellwords;
use super::special_commands::parse_export_command;
use cmd::{CommandNode, NodeArg};
use dash::graph::command as cmd;
use dash::graph::read2 as read;
use dash::graph::write2 as write;
use dash::graph::{filestream, info, program, rapper, stream, Location};
use dash::util::Result;
use failure::bail;
use filestream::{FileMode, FileStream};
use info::Info;
use itertools::join;
use program::{Elem, NodeId, Program};
use rapper::Rapper;
use read::ReadNode;
use serde::{Deserialize, Serialize};
use shellwords::split;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use stream::{DashStream, IOType, PipeStream};
use write::WriteNode;

// General types
pub enum Command {
    /// just an export command.
    EXPORT(String, String),
    /// Program that needs to be parsed
    PROGRAM(Program),
}

pub fn parse_command(command: &str) -> Result<Command> {
    if command.starts_with("export") {
        let (var, value) = parse_export_command(command)?;
        Ok(Command::EXPORT(var, value))
    } else {
        // make a shell split from the command
        let shellsplit = ShellSplit::new(command)?;
        // turn shell split into shell graph
        let shellgraph = shellsplit.convert_into_shell_graph()?;
        // turn into program that interpreter can deal with
        let program = shellgraph.convert_into_program()?;
        Ok(Command::PROGRAM(program))
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct SubCommand {
    pub elts: Vec<RawShellElement>,
}

impl SubCommand {
    pub fn new(elts: Vec<RawShellElement>) -> Self {
        SubCommand { elts: elts }
    }

    pub fn push(&mut self, elt: RawShellElement) {
        self.elts.push(elt);
    }

    /// Generates a string representation of the SubCommand.
    /// Used for debugging.
    pub fn to_string(&self) -> String {
        let display_strings: Vec<String> = self.elts.iter().map(|x| x.to_string()).collect();
        join(display_strings, " ")
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct ShellGraphNode {
    pub cmd: SubCommand,
    pub id: NodeId,
}

impl ShellGraphNode {
    pub fn to_string(&self) -> String {
        format!("{:?}: {:?}", self.id, self.cmd.to_string())
    }
    pub fn push(&mut self, elt: RawShellElement) {
        self.cmd.push(elt);
    }

    /// generates a program node from the list of raw shell elements.
    /// Assumes all subcommands have been parsed already, JUST handles file redirections for stdin,
    /// stderr, and stdout.
    // Merges all the subgraphs for the piped programs,
    // handles any links that need to be made, and adds any redirection to local Stdout/Stdin.
    pub fn generate_subprogram(&self) -> Result<Program> {
        let mut new_program = Program::default();
        let mut cmd_node = CommandNode::default();
        let mut iter = self.cmd.elts.iter();
        let mut stdin_nodes: Vec<ReadNode> = Vec::new();
        let mut stdout_nodes: Vec<WriteNode> = Vec::new();
        let mut stderr_nodes: Vec<WriteNode> = Vec::new();

        while let Some(elt) = iter.next() {
            match elt {
                RawShellElement::Str(word) => {
                    // is it a safe assumption that the command is always at the front of list?
                    if !cmd_node.name_set() {
                        cmd_node.set_name(word);
                    } else {
                        cmd_node.add_arg(NodeArg::Str(word.to_string()));
                    }
                }
                RawShellElement::Stdin => {
                    // look for the next argument, and add a *READ NODE* prior to this node
                    if let Some(next_elt) = iter.next() {
                        match next_elt {
                            RawShellElement::Str(filename) => {
                                let mut readnode = ReadNode::default();
                                readnode.add_stdin(DashStream::File(FileStream::new(
                                    Path::new(&filename),
                                    Location::Client,
                                )))?;
                                stdin_nodes.push(readnode);
                            }
                            _ => {
                                bail!("Stdin in this stage can only be followed by strings");
                            }
                        }
                    } else {
                        bail!("Stdin directive without anything following!");
                    }
                }
                RawShellElement::Stdout => {
                    if let Some(next_elt) = iter.next() {
                        match next_elt {
                            RawShellElement::Str(filename) => {
                                let mut writenode = WriteNode::default();
                                writenode.set_stdout(DashStream::File(FileStream::new(
                                    Path::new(&filename),
                                    Location::Client,
                                )))?;
                                stdout_nodes.push(writenode);
                            }
                            _ => {
                                bail!("Stdout in this stage can only be followed by strings");
                            }
                        }
                    } else {
                        bail!("Stdout directive without anything following!");
                    }
                }
                RawShellElement::StdoutAppend => {
                    if let Some(next_elt) = iter.next() {
                        match next_elt {
                            RawShellElement::Str(filename) => {
                                let mut writenode = WriteNode::default();
                                let mut fs =
                                    FileStream::new(Path::new(&filename), Location::Client);
                                fs.set_mode(FileMode::APPEND);
                                writenode.set_stdout(DashStream::File(fs))?;
                                stdout_nodes.push(writenode);
                            }
                            _ => {
                                bail!("Stdout in this stage can only be followed by strings");
                            }
                        }
                    } else {
                        bail!("Stdout directive without anything following!");
                    }
                }
                RawShellElement::Stderr => {
                    if let Some(next_elt) = iter.next() {
                        match next_elt {
                            RawShellElement::Str(filename) => {
                                let mut writenode = WriteNode::default();
                                // Write nodes that write to stderr still consider output as
                                // `stdout`
                                // Only *cmdnodes* have `stderr` output
                                writenode.set_stdout(DashStream::File(FileStream::new(
                                    Path::new(&filename),
                                    Location::Client,
                                )))?;
                                stderr_nodes.push(writenode);
                            }
                            _ => {
                                bail!("Stderr in this stage can only be followed by strings");
                            }
                        }
                    } else {
                        bail!("Stderr directive without anything following!");
                    }
                }
                RawShellElement::Pipe => {
                    bail!("Should not encounter a pipe when generating a subprogram from a shell graph node, all pipes should be parsed already");
                }
                RawShellElement::Subcmd(subcmd) => {
                    bail!("Should not encounter subcommand when generating a subprogram from a shell graph node: {:?}", subcmd);
                }
            }
        }

        let cmd_node_id = new_program.add_elem(Elem::Cmd(cmd_node));
        for stdin in stdin_nodes.into_iter() {
            // insert both nodes into the graph, and add an edge
            let stdin_node_id = new_program.add_elem(Elem::Read(stdin));
            // create a new pipe between the read node and command node, and add it to both nodes
            let pipe = PipeStream::new(stdin_node_id, cmd_node_id, IOType::Stdout)?;
            // add an edge between the two nodes
            new_program.add_unique_edge(stdin_node_id, cmd_node_id);
            // add the pipe to the stdout/stdin of both nodes
            let cmd_elem = new_program
                .get_mut_node(cmd_node_id)
                .unwrap()
                .get_mut_elem();
            cmd_elem.add_stdin(DashStream::Pipe(pipe.clone()))?;
            let stdin_elem = new_program
                .get_mut_node(stdin_node_id)
                .unwrap()
                .get_mut_elem();
            stdin_elem.add_stdout(DashStream::Pipe(pipe))?;
            new_program.add_unique_edge(stdin_node_id, cmd_node_id);
        }
        for stdout in stdout_nodes.into_iter() {
            let stdout_node_id = new_program.add_elem(Elem::Write(stdout));
            let pipe = PipeStream::new(cmd_node_id, stdout_node_id, IOType::Stdout)?;
            new_program.add_unique_edge(cmd_node_id, stdout_node_id);
            let cmd_elem = new_program
                .get_mut_node(cmd_node_id)
                .unwrap()
                .get_mut_elem();
            cmd_elem.add_stdout(DashStream::Pipe(pipe.clone()))?;
            let stdout_elem = new_program
                .get_mut_node(stdout_node_id)
                .unwrap()
                .get_mut_elem();
            stdout_elem.add_stdin(DashStream::Pipe(pipe))?;
            new_program.add_unique_edge(cmd_node_id, stdout_node_id);
        }
        for stderr in stderr_nodes.into_iter() {
            let stderr_node_id = new_program.add_elem(Elem::Write(stderr));
            let pipe = PipeStream::new(cmd_node_id, stderr_node_id, IOType::Stderr)?;
            new_program.add_unique_edge(cmd_node_id, stderr_node_id);
            let cmd_elem = new_program
                .get_mut_node(cmd_node_id)
                .unwrap()
                .get_mut_elem();
            cmd_elem.add_stderr(DashStream::Pipe(pipe.clone()))?;
            let stderr_elem = new_program
                .get_mut_node(stderr_node_id)
                .unwrap()
                .get_mut_elem();
            stderr_elem.add_stdin(DashStream::Pipe(pipe))?;
            new_program.add_unique_edge(cmd_node_id, stderr_node_id);
        }
        Ok(new_program)
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub struct ShellLink {
    pub left: NodeId,
    pub right: NodeId,
}

/// Representation of ShellGraph as a connection of piped processes.
/// Links in this graph represents processes that pipe STDOUT together.
/// File redirections of stdin, stdout and stderr have not been parsed together yet.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct ShellGraph {
    pub nodes: HashMap<NodeId, ShellGraphNode>,
    pub edges: Vec<ShellLink>,
    counter: u32,
    sinks: Vec<NodeId>,
    front: Vec<NodeId>,
}

impl Default for ShellGraph {
    fn default() -> Self {
        ShellGraph {
            nodes: HashMap::default(),
            edges: vec![],
            counter: 0,
            sinks: vec![],
            front: vec![],
        }
    }
}

impl ShellGraph {
    fn get_node_string(&self, node_id: &NodeId) -> Result<String> {
        match self.nodes.get(node_id) {
            Some(n) => Ok(n.to_string()),
            None => bail!("Did not find node id : {:?} in graph"),
        }
    }

    /// Writes dot representation of this graph to a file for visualization.
    pub fn write_dot(&self, filename: &str) -> Result<()> {
        let mut file = File::create(filename)?;
        // header
        file.write_all(b"digraph {\n")?;
        // iterate through edges and make a line for each
        for edge in self.edges.iter() {
            let left_string = self.get_node_string(&edge.left)?;
            let right_string = self.get_node_string(&edge.right)?;
            file.write_fmt(format_args!("{:?} -> {:?}\n", left_string, right_string))?;
        }
        file.write_all(b"}")?;
        // end
        Ok(())
    }
    fn add_node(&mut self, cmd: SubCommand) -> NodeId {
        let node = ShellGraphNode {
            cmd: cmd,
            id: self.counter,
        };
        self.nodes.insert(self.counter, node);
        self.counter += 1;
        self.sinks.push(self.counter - 1);
        self.front.push(self.counter - 1);
        return self.counter - 1;
    }

    fn get_node(&mut self, id: NodeId) -> Option<&mut ShellGraphNode> {
        self.nodes.get_mut(&id)
    }

    fn add_link(&mut self, left: NodeId, right: NodeId) {
        if self.sinks.contains(&left) {
            self.sinks.retain(|&x| x != left);
        }
        if self.front.contains(&right) {
            self.front.retain(|&x| x != right);
        }
        self.edges.push(ShellLink {
            left: left,
            right: right,
        });
    }

    // finds "right most node"
    pub fn get_end(&self) -> Vec<NodeId> {
        self.sinks.clone()
    }

    // finds the "left most node"
    pub fn get_front(&self) -> Vec<NodeId> {
        self.front.clone()
    }

    pub fn get_subgraph_map(&self) -> Result<HashMap<NodeId, Program>> {
        let mut subgraph_map: HashMap<NodeId, Program> = HashMap::default();
        for (id, graph_node) in self.nodes.iter() {
            let subgraph = graph_node.generate_subprogram()?;
            subgraph_map.insert(*id, subgraph);
        }
        Ok(subgraph_map)
    }

    pub fn get_program_without_output_nodes(&self) -> Result<Program> {
        // generate subgraphs for each part
        let subgraph_map = self.get_subgraph_map()?;

        let mut links: Vec<((NodeId, NodeId), (NodeId, NodeId))> = Vec::new();
        let mut old_links: Vec<((NodeId, NodeId), (NodeId, NodeId))> = Vec::new();
        // any links present in subgraphs
        for (id, subgraph) in subgraph_map.iter() {
            for edge in subgraph.get_edges_iter() {
                old_links.push(((*id, edge.get_left()), (*id, edge.get_right())));
            }
        }

        // connect subgraphs by pipe via adding a new edge.
        for edge in self.edges.iter() {
            // connect node 0 of each new subgraph
            links.push(((edge.left, 1), (edge.right, 1)));
        }
        // merge all subgraphs into 1 program
        let program = Program::merge_subgraphs(subgraph_map, old_links, links)?;
        Ok(program)
    }

    /// Takes the shell graph, which represents processes linked together by pipes,
    /// and turns it into the program Graph.
    /// This step parses any file redirections for stdin, stdout and stderr file redirections.
    /// Note that we only handle a couple of small cases.
    pub fn convert_into_program(&self) -> Result<Program> {
        // generate subgraphs for each part
        let subgraph_map = self.get_subgraph_map()?;

        let mut links: Vec<((NodeId, NodeId), (NodeId, NodeId))> = Vec::new();
        let mut old_links: Vec<((NodeId, NodeId), (NodeId, NodeId))> = Vec::new();
        // any links present in subgraphs
        for (id, subgraph) in subgraph_map.iter() {
            for edge in subgraph.get_edges_iter() {
                old_links.push(((*id, edge.get_left()), (*id, edge.get_right())));
            }
        }

        // connect subgraphs by pipe via adding a new edge.
        for edge in self.edges.iter() {
            // connect node 0 of each new subgraph
            links.push(((edge.left, 1), (edge.right, 1)));
        }
        // merge all subgraphs into 1 program
        let mut program = Program::merge_subgraphs(subgraph_map, old_links, links)?;
        // now, go through and add in stdout and stderr redirections for any nodes that do not
        // have any redirection currently
        let mut add_output_nodes: Vec<(NodeId, IOType)> = Vec::new();
        for (id, node) in program.get_nodes_iter() {
            match node.get_elem() {
                Elem::Cmd(_) => {}
                _ => continue,
            }
            if node.get_stdout_len() == 0 {
                add_output_nodes.push((*id, IOType::Stdout));
            }
            if node.get_stderr_len() == 0 {
                add_output_nodes.push((*id, IOType::Stderr));
            }
        }
        for (id, iotype) in add_output_nodes.iter() {
            let mut writenode = WriteNode::default();
            match iotype {
                IOType::Stdout => {
                    writenode.set_stdout(DashStream::Stdout)?;
                }
                IOType::Stderr => {
                    // Note: write nodes 'output' is set with set_stdout
                    writenode.set_stdout(DashStream::Stderr)?;
                }
                _ => bail!("We shouldn't be adding a link to stdout"),
            }
            let new_id = program.add_elem(Elem::Write(writenode));
            let new_pipe = PipeStream::new(*id, new_id, *iotype)?;
            // now add the pipestream to the actual node
            match iotype {
                IOType::Stdout => {
                    program
                        .get_mut_node(*id)
                        .unwrap()
                        .add_stdout(DashStream::Pipe(new_pipe.clone()))?;
                }

                IOType::Stderr => {
                    program
                        .get_mut_node(*id)
                        .unwrap()
                        .add_stderr(DashStream::Pipe(new_pipe.clone()))?;
                }
                _ => {}
            }
            program
                .get_mut_node(new_id)
                .unwrap()
                .add_stdin(DashStream::Pipe(new_pipe.clone()))?;
            program.add_unique_edge(*id, new_id);
        }

        Ok(program)
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// Merges two shell graphs into 1 shell graph.
    /// Connects the other graph to this graph with the given ShellLink.
    /// If is_input is true, puts the other graph as input to this graph;
    /// if false, puts the other graph as output.
    /// Returns Error if the node Ids specified in the ShellLink are in neither graph.
    pub fn merge(
        &mut self,
        other: ShellGraph,
        connection_link: Option<(ShellLink, bool)>,
    ) -> Result<()> {
        // Check if both graphs contain the nodes on the given link.
        let mut id_map: HashMap<NodeId, NodeId> = HashMap::default();
        for (old_id, node) in other.nodes.iter() {
            let new_id = self.add_node(node.cmd.clone());
            id_map.insert(old_id.clone(), new_id);
        }

        // add all the old links
        for link in other.edges.iter() {
            self.add_link(
                id_map.get(&link.left).unwrap().clone(),
                id_map.get(&link.right).unwrap().clone(),
            );
        }

        // add in the connection
        match connection_link {
            Some((link, is_input)) => {
                // check both nodes exist, and add the link
                if is_input {
                    if !self.contains(link.right) || !other.contains(link.left) {
                        bail!(
                            "Self does not contain left side of pipe: {:?}, nodes: {:?}",
                            link,
                            self.nodes.keys()
                        );
                    }
                    self.add_link(id_map.get(&link.left).unwrap().clone(), link.right.clone());
                } else {
                    if !self.contains(link.left) || !other.contains(link.right) {
                        bail!(
                            "Self does not contain right side of pipe: {:?}, nodes: {:?}, other nodes: {:?}",
                            link,
                            self.nodes.keys(),
                            other.nodes.keys(),
                        );
                    }
                    self.add_link(link.left.clone(), id_map.get(&link.right).unwrap().clone());
                }
            }
            None => {}
        }
        Ok(())
    }
}
/// Very initial parse of command divides the command into the list of the following.
/// Because this shell level parser is not full featured, we don't support nested subcommands.
#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Hash, Eq)]
pub enum RawShellElement {
    Str(String),
    Stdin,
    Stdout,
    Stderr,
    Pipe,
    StdoutAppend,
    Subcmd(SubCommand),
}

impl RawShellElement {
    pub fn to_string(&self) -> String {
        match &*self {
            RawShellElement::Str(string) => string.clone(),
            RawShellElement::Stdin => "<".to_string(),
            RawShellElement::Stdout => ">".to_string(),
            RawShellElement::Stderr => "2>".to_string(),
            RawShellElement::Pipe => "|".to_string(),
            RawShellElement::StdoutAppend => ">>".to_string(),
            RawShellElement::Subcmd(cmd) => cmd.to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
pub struct ShellSplit {
    elts: Vec<RawShellElement>,
}

impl ShellSplit {
    /// Divides the command into the intermediary shell representation, a vector of shell elements.
    pub fn from_vec(elts: Vec<RawShellElement>) -> Self {
        ShellSplit { elts: elts }
    }
    pub fn new(cmd: &str) -> Result<Self> {
        let shell_split = match split(&cmd) {
            Ok(s) => s,
            Err(e) => bail!("Mismatched quotes error: {:?}", e),
        };

        let mut elements: Vec<RawShellElement> = Vec::new();
        let mut it = shell_split.iter();
        while let Some(elt) = it.next() {
            // first look for a subcommand
            match elt.as_ref() {
                "<(" => {
                    let mut found_close_parens = false;
                    let mut subcommand: Vec<RawShellElement> = Vec::new();
                    while let Some(inner_elt) = it.next() {
                        match inner_elt.as_ref() {
                            ")" => {
                                found_close_parens = true;
                            }
                            _ => {}
                        }
                        if found_close_parens {
                            break;
                        }
                        match inner_elt.as_ref() {
                            ">" => {
                                subcommand.push(RawShellElement::Stdout);
                            }
                            ">>" => {
                                subcommand.push(RawShellElement::StdoutAppend);
                            }
                            "<" => {
                                subcommand.push(RawShellElement::Stdin);
                            }
                            "2>" => {
                                subcommand.push(RawShellElement::Stderr);
                            }
                            "|" => {
                                subcommand.push(RawShellElement::Pipe);
                            }
                            _ => {
                                subcommand.push(RawShellElement::Str(inner_elt.clone()));
                            }
                        }
                    }
                    if !found_close_parens {
                        bail!("Unclosed parens!");
                    }
                    elements.push(RawShellElement::Stdin);
                    elements.push(RawShellElement::Subcmd(SubCommand::new(subcommand)));
                }
                "<" => {
                    elements.push(RawShellElement::Stdin);
                }
                ">" => {
                    elements.push(RawShellElement::Stdout);
                }
                ">>" => {
                    elements.push(RawShellElement::StdoutAppend);
                }
                "2>" => {
                    elements.push(RawShellElement::Stderr);
                }
                "|" => {
                    elements.push(RawShellElement::Pipe);
                }
                _ => {
                    // resolve any environment variables

                    elements.push(RawShellElement::Str(elt.clone()));
                }
            }
        }
        Ok(ShellSplit { elts: elements })
    }

    /// Takes the Shell Split and converts it into a graph.
    pub fn convert_into_shell_graph(&self) -> Result<ShellGraph> {
        let mut graph = ShellGraph::default();
        // first, split everything by pipe, then make everything a subcommand
        let mut parts = self.elts.split(|elt| elt.clone() == RawShellElement::Pipe);
        // merge all parts into the top level graph.
        while let Some(subcmd) = parts.next() {
            //tracing::debug!("next part: {:?}", subcmd);
            let new_subgraph = get_subgraph(subcmd)?;
            //tracing::debug!("new subgraph: {:?}", new_subgraph);
            if graph.nodes.len() == 0 {
                /*tracing::debug!(
                    "current graph nodes: {:?}, subgraph: {:?}",
                    graph.nodes.keys(),
                    new_subgraph.nodes.keys()
                );*/
                graph.merge(new_subgraph, None)?;
            //tracing::debug!("new graph nodes: {:?}", graph.nodes.keys());
            } else {
                // TODO: this accessing of the first value of front and sink doesn't really scale
                let graph_end = graph.get_end()[0];
                let subgraph_front = new_subgraph.get_front()[0];
                /*tracing::debug!(
                    "current graph nodes: {:?}, subgraph: {:?}",
                    graph.nodes.keys(),
                    new_subgraph.nodes.keys()
                );
                tracing::debug!(
                    "proposed link: {:?}",
                    ShellLink {
                        left: graph_end,
                        right: subgraph_front
                    }
                );*/
                graph.merge(
                    new_subgraph,
                    Some((
                        ShellLink {
                            left: graph_end,
                            right: subgraph_front,
                        },
                        false,
                    )),
                )?;
                //tracing::debug!("new graph nodes: {:?}", graph.nodes.keys());
            }
        }
        Ok(graph)
    }
}

fn get_subgraph(subcmd: &[RawShellElement]) -> Result<ShellGraph> {
    // Takes out any internal pipes stdout directives
    let mut graph = ShellGraph::default();
    let mut it = subcmd.iter();
    let id = graph.add_node(SubCommand::new(Vec::<RawShellElement>::new()));
    while let Some(elt) = it.next() {
        match elt.clone() {
            RawShellElement::Str(cmd) => {
                let current_node = graph.get_node(id).unwrap();
                current_node.push(RawShellElement::Str(cmd.clone()));
            }
            RawShellElement::Stdin => {
                // check if the next elt is a subcommand
                if let Some(next_elt) = it.next() {
                    match next_elt.clone() {
                        RawShellElement::Str(cmd) => {
                            let current_node = graph.get_node(id).unwrap();
                            current_node.push(RawShellElement::Stdin);
                            current_node.push(RawShellElement::Str(cmd));
                        }
                        RawShellElement::Subcmd(subcmd) => {
                            // get a shell graph for the subcommand, and insert it into the current
                            // graph
                            let new_shell_split = ShellSplit::from_vec(subcmd.elts);
                            let new_subgraph = new_shell_split.convert_into_shell_graph()?;
                            let sink_id = new_subgraph.get_end()[0];
                            graph.merge(
                                new_subgraph,
                                Some((
                                    ShellLink {
                                        left: sink_id,
                                        right: id,
                                    },
                                    true,
                                )),
                            )?;
                        }
                        _ => {
                            bail!("Found stdin symbol followed by stdin, stdout, or stderr symbol");
                        }
                    }
                } else {
                    bail!("Stdin directive with nothing following");
                }
            }
            RawShellElement::Stderr => {
                let current_node = graph.get_node(id).unwrap();
                current_node.push(RawShellElement::Stderr);
            }
            RawShellElement::StdoutAppend => {
                let current_node = graph.get_node(id).unwrap();
                current_node.push(RawShellElement::StdoutAppend);
            }
            RawShellElement::Stdout => {
                let current_node = graph.get_node(id).unwrap();
                current_node.push(RawShellElement::Stdout);
            }
            RawShellElement::Subcmd(subcmd) => {
                bail!(
                    "Currently can only handle subcommands that follow stdin symbols: {:?}",
                    subcmd
                );
            }
            RawShellElement::Pipe => {
                bail!("Shouldn't have nested pipes");
            }
        }
    }
    Ok(graph)
}

#[cfg(test)]
// TODO: FIGURE OUT HOW TO TEST THIS FOR REAL
mod test {
    use super::*;
    //use std::collections::hash_map::Iter as HashIter;
    //use std::slice::Iter as SliceIter;
    //

    #[test]
    fn test_mogrify() {
        let cmd = "mogrify  -format gif -path thumbs_dir -thumbnail 100x100 data_dir/*.jpg";
        let shell_split = ShellSplit::new(cmd).unwrap();
        let shell_prog = shell_split.convert_into_shell_graph().unwrap();
        let program = shell_prog.convert_into_program().unwrap();
        tracing::debug!("program: {:?}", program);
    }

    #[test]
    fn test_shell_split_to_graph() {
        let cmd = "grep foo <( cat bar ) <( cat bar2 ) | wc > output.txt 2> err.txt";
        match ShellSplit::new(cmd) {
            Ok(shell_split) => match shell_split.convert_into_shell_graph() {
                Ok(shell_prog) => {
                    tracing::debug!("shell prog: {:?}", shell_prog);
                    tracing::debug!("________");
                    match shell_prog.convert_into_program() {
                        Ok(p) => {
                            tracing::debug!("Program!");
                            tracing::debug!("{:?}", p);
                            //assert!(false);
                        }
                        Err(e) => {
                            tracing::debug!("Failed to convert shell split into program: {:?}", e);
                            assert!(false);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!("Failed to convert split into program: {:?}", e);
                    assert!(false);
                }
            },
            Err(e) => {
                tracing::debug!("Failed to divide into shell split: {:?}", e);
                assert!(false);
            }
        }
    }

    #[test]
    fn test_scan_command() {
        let cmd = "pr -mts, <( cat annotated | jq \".ip\" | tr -d '\"' ) <( cat annotated | jq -c \".zannotate.routing.asn\" ) | awk -F',' '{ a[$2]++; } END { for (n in a) print n \",\" a[n] } ' | sort -k2 -n -t',' -r > as_popularity";
        match ShellSplit::new(cmd) {
            Ok(shell_split) => match shell_split.convert_into_shell_graph() {
                Ok(shell_prog) => {
                    tracing::debug!("Prog: {:?}", shell_prog);
                }
                Err(e) => {
                    tracing::debug!("{:?}", e);
                }
            },
            Err(e) => {
                tracing::debug!("Failed to parse command into shell split: {:?}", e);
                assert!(false);
            }
        };
    }
}
