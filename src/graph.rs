//! A small mirror of the PipeWire graph, built from `pw-dump --monitor`.
//!
//! `pw-dump --monitor` prints a JSON array of objects whenever something
//! changes. Each object has an `id`, a `type` (Client, Node, Link, ...) and
//! an `info` block. A removed object comes through with `"info": null`.
//!
//! We only keep what we need: clients (they carry the real PID), nodes (the
//! streams and sinks) and which playback streams we've seen linked.

use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

pub type Props = Map<String, Value>;

/// Something interesting happened that `main` should react to.
#[derive(Debug)]
pub enum Event {
    /// A new app playback stream appeared.
    NewPlayback(u32),
    /// A playback stream got linked to a sink (first link only).
    Linked { stream: u32, sink: u32 },
    /// An object went away.
    Removed(u32),
}

#[derive(Default)]
pub struct Graph {
    clients: HashMap<u32, Props>,
    nodes: HashMap<u32, Props>,
    linked: HashSet<u32>,
}

impl Graph {
    /// Feed one object from pw-dump into the graph.
    pub fn apply(&mut self, obj: &Value) -> Vec<Event> {
        let mut events = Vec::new();

        let Some(id) = obj.get("id").and_then(Value::as_u64).map(|n| n as u32) else {
            return events;
        };

        // Removal: info is null or missing.
        let info = match obj.get("info") {
            Some(info) if !info.is_null() => info,
            _ => {
                self.clients.remove(&id);
                self.nodes.remove(&id);
                self.linked.remove(&id);
                events.push(Event::Removed(id));
                return events;
            }
        };

        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "PipeWire:Interface:Client" => {
                if let Some(props) = props_of(info) {
                    self.clients.insert(id, props);
                }
            }
            "PipeWire:Interface:Node" => {
                if let Some(props) = props_of(info) {
                    let is_new = !self.nodes.contains_key(&id);
                    let is_playback =
                        str_prop(&props, "media.class") == Some("Stream/Output/Audio");
                    self.nodes.insert(id, props);
                    if is_new && is_playback {
                        events.push(Event::NewPlayback(id));
                    }
                }
            }
            "PipeWire:Interface:Link" => {
                let out = info.get("output-node-id").and_then(Value::as_u64);
                let inp = info.get("input-node-id").and_then(Value::as_u64);
                if let (Some(out), Some(inp)) = (out, inp) {
                    let (out, inp) = (out as u32, inp as u32);
                    // `insert` returns true only the first time, so each
                    // stream reports one link even though FL and FR each
                    // get their own link object.
                    if self.is_playback(out) && self.linked.insert(out) {
                        events.push(Event::Linked {
                            stream: out,
                            sink: inp,
                        });
                    }
                }
            }
            _ => {}
        }
        events
    }

    fn is_playback(&self, node: u32) -> bool {
        self.nodes
            .get(&node)
            .is_some_and(|p| str_prop(p, "media.class") == Some("Stream/Output/Audio"))
    }

    /// Look up a property on a node, falling back to its client.
    /// Apps often set e.g. `application.name` on only one of the two.
    pub fn prop(&self, node: u32, key: &str) -> Option<String> {
        let node_props = self.nodes.get(&node)?;
        if let Some(v) = node_props.get(key).and_then(value_to_string) {
            return Some(v);
        }
        self.client_of(node)?.get(key).and_then(value_to_string)
    }

    /// Only from the node itself (no client fallback).
    pub fn node_prop(&self, node: u32, key: &str) -> Option<String> {
        self.nodes.get(&node)?.get(key).and_then(value_to_string)
    }

    fn client_of(&self, node: u32) -> Option<&Props> {
        let client_id = self.node_prop(node, "client.id")?.parse::<u32>().ok()?;
        self.clients.get(&client_id)
    }

    /// The process ID behind a stream. `pipewire.sec.pid` comes from the
    /// socket itself so it's trustworthy, and it's the real PID even for
    /// Flatpak apps. `application.process.id` is what the app claims.
    pub fn pid_of(&self, node: u32) -> Option<u32> {
        let from_client = self
            .client_of(node)
            .and_then(|c| c.get("pipewire.sec.pid"))
            .and_then(value_to_string);
        from_client
            .or_else(|| self.prop(node, "application.process.id"))
            .and_then(|s| s.parse().ok())
    }

    /// PIDs of every app that currently has a microphone (capture) stream.
    pub fn capture_pids(&self) -> HashSet<u32> {
        self.nodes
            .iter()
            .filter(|(_, p)| str_prop(p, "media.class") == Some("Stream/Input/Audio"))
            .filter_map(|(id, _)| self.pid_of(*id))
            .collect()
    }
}

fn props_of(info: &Value) -> Option<Props> {
    info.get("props")?.as_object().cloned()
}

fn str_prop<'a>(props: &'a Props, key: &str) -> Option<&'a str> {
    props.get(key)?.as_str()
}

/// pw-dump writes some values as numbers and some as strings; we want text.
fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
