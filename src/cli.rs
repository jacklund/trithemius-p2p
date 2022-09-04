use clap::{error::ErrorKind, ValueEnum};
use libp2p::{rendezvous::Namespace, PeerId};
use std::fmt::Display;
use std::str::FromStr;

#[derive(Copy, Clone, Display, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Discovery {
    Kademlia,
    Mdns,
    Rendezvous,
}

#[derive(Copy, Clone, Display, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum NatTraversal {
    Autonat,
    CircuitRelay,
    Dcutr,
}

#[derive(Clone, Debug)]
pub struct NamespaceAndNodeId {
    pub namespace: Namespace,
    pub node_id: PeerId,
}

impl clap::builder::ValueParserFactory for NamespaceAndNodeId {
    type Parser = NamespaceAndNodeIdParser;
    fn value_parser() -> Self::Parser {
        NamespaceAndNodeIdParser
    }
}

#[derive(Clone, Debug)]
pub struct NamespaceAndNodeIdParser;

impl clap::builder::TypedValueParser for NamespaceAndNodeIdParser {
    type Value = NamespaceAndNodeId;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let mut cmd = cmd.clone();
        match value.to_str() {
            Some(value_str) => {
                if value_str.contains('/') {
                    let values = value_str.split_once('/').unwrap();
                    let namespace = match Namespace::new(values.0.to_string()) {
                        Ok(namespace) => namespace,
                        Err(_) => {
                            return Err(
                                cmd.error(ErrorKind::ValueValidation, "Namespace is too long")
                            )
                        }
                    };
                    let node_id = match PeerId::from_str(values.1) {
                        Ok(node_id) => node_id,
                        Err(error) => {
                            return Err(cmd.error(
                                ErrorKind::ValueValidation,
                                format!("Error parsing peer ID: {}", error),
                            ))
                        }
                    };
                    Ok(Self::Value { namespace, node_id })
                } else {
                    Err(cmd.error(
                        ErrorKind::ValueValidation,
                        "Namespace and NodeId should be specified as 'NAMESPACE/NODE_ID'",
                    ))
                }
            }
            None => Err(cmd.error(
                ErrorKind::ValueValidation,
                "Namespace and NodeId aren't valid unicode",
            )),
        }
    }
}
