use std::collections::BTreeMap;

pub const LIMIT: usize = 8;

pub struct Graph {
    pub nodes: BTreeMap<String, usize>,
}

pub enum Kind {
    Node,
    Edge,
}

pub trait Visitor {
    fn visit(&self, name: &str) -> bool;
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            nodes: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, name: &str) {
        self.check(name);
    }

    fn check(&self, name: &str) -> bool {
        !name.is_empty()
    }
}

mod inner {
    pub fn helper() -> usize {
        compute()
    }
}

fn compute() -> usize {
    1
}
