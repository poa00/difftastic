#![allow(dead_code)]

use std::cmp::Ordering;
use std::collections::HashSet;
use std::collections::{BinaryHeap, HashMap};
use std::hash::{Hash, Hasher};

use crate::tree_diff::Node;
use Action::*;

#[derive(Debug, Eq, Clone)]
struct GraphNode<'a> {
    distance: u64,
    action: Action,
    lhs_next: Option<(&'a Node<'a>, Vec<usize>)>,
    rhs_next: Option<(&'a Node<'a>, Vec<usize>)>,
}

impl<'a> GraphNode<'a> {
    fn new(lhs: &'a Node<'a>, rhs: &'a Node<'a>) -> Self {
        Self {
            distance: 0,
            // TODO: this makes no sense for the source node.
            action: Action::UnchangedNode,
            lhs_next: Some((lhs, vec![0])),
            rhs_next: Some((rhs, vec![0])),
        }
    }

    fn lhs_next_node(&self) -> Option<&'a Node<'a>> {
        self.lhs_next.as_ref().map(|(n, _)| *n)
    }
    fn rhs_next_node(&self) -> Option<&'a Node<'a>> {
        self.rhs_next.as_ref().map(|(n, _)| *n)
    }

    fn is_end(&self) -> bool {
        self.lhs_next.is_none() && self.rhs_next.is_none()
    }
}

// Rust requires that PartialEq, PartialOrd and Ord agree.
// https://doc.rust-lang.org/std/cmp/trait.Ord.html
//
// For GraphNode, we want to compare by distance in a priority queue,
// but equality should only consider LHS/RHS node when deciding if
// we've visited a node. We define separate wrappers for these two use
// cases.
#[derive(Debug, Eq)]
struct OrderedGraphNode<'a> {
    gn: GraphNode<'a>,
}

impl<'a> PartialOrd for OrderedGraphNode<'a> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<'a> Ord for OrderedGraphNode<'a> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.gn.distance.cmp(&other.gn.distance)
    }
}

impl<'a> PartialEq for OrderedGraphNode<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.gn.distance == other.gn.distance
    }
}

// A GraphNode that only considers the underlying Nodes for equality,
// ignoring distance.
#[derive(Debug)]
struct EqualityGraphNode<'a> {
    gn: GraphNode<'a>,
}

impl<'a> PartialEq for EqualityGraphNode<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.gn.lhs_next_node() == other.gn.lhs_next_node()
            && self.gn.rhs_next_node() == other.gn.rhs_next_node()
    }
}
impl<'a> Eq for EqualityGraphNode<'a> {}

impl<'a> Hash for EqualityGraphNode<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.gn.lhs_next_node().hash(state);
        self.gn.rhs_next_node().hash(state);
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum Action {
    UnchangedNode,
    UnchangedDelimiter,
    NovelAtomLHS,
    NovelAtomRHS,
    NovelDelimiterLHS,
    NovelDelimiterRHS,
}

impl Action {
    fn cost(&self) -> u64 {
        match self {
            // Matching nodes is always best.
            UnchangedNode => 0,
            // Matcing an outer delimiter is good.
            UnchangedDelimiter => 1,
            // Otherwise, we've added/removed a node.
            NovelAtomLHS => 2,
            NovelAtomRHS => 2,
            NovelDelimiterLHS => 2,
            NovelDelimiterRHS => 2,
        }
    }
}

fn find_route<'a>(start: GraphNode<'a>) -> Vec<GraphNode<'a>> {
    let mut heap = BinaryHeap::new();
    heap.push(OrderedGraphNode { gn: start.clone() });

    let mut visited: HashSet<EqualityGraphNode> = HashSet::new();
    let mut predecessors: HashMap<EqualityGraphNode, GraphNode> = HashMap::new();

    let end;
    'outer: loop {
        match heap.pop() {
            Some(ogn) => {
                let egn = EqualityGraphNode { gn: ogn.gn };
                if visited.contains(&egn) {
                    continue;
                }

                let gn = egn.gn;
                for new_gn in next_graph_nodes(&gn) {
                    predecessors.insert(EqualityGraphNode { gn: new_gn.clone() }, gn.clone());

                    if new_gn.is_end() {
                        end = new_gn;
                        break 'outer;
                    }

                    heap.push(OrderedGraphNode { gn: new_gn });
                }

                visited.insert(EqualityGraphNode { gn });
            }
            None => panic!("Ran out of graph nodes before reaching end"),
        }
    }

    let mut current = end;
    let mut res = vec![];
    loop {
        res.push(current.clone());

        match predecessors.remove(&EqualityGraphNode {
            gn: current.clone(),
        }) {
            Some(node) => {
                current = node;
            }
            None => break,
        }
    }

    res.reverse();
    res
}

fn next_graph_nodes<'a>(gn: &GraphNode<'a>) -> Vec<GraphNode<'a>> {
    let mut res = vec![];

    match (&gn.lhs_next, &gn.rhs_next) {
        (Some((lhs_next_node, lhs_next_idx)), Some((rhs_next_node, rhs_next_idx))) => {
            if lhs_next_node == rhs_next_node {
                // Both nodes are equal, the happy case.
                let action = UnchangedNode;
                res.push(GraphNode {
                    action,
                    distance: gn.distance + action.cost(),
                    lhs_next: next_node(lhs_next_node, lhs_next_idx.clone()),
                    rhs_next: next_node(rhs_next_node, rhs_next_idx.clone()),
                });
            }
        }
        _ => {}
    }

    if let Some((lhs_next_node, lhs_next_idx)) = &gn.lhs_next {
        match lhs_next_node {
            // Step over this novel atom.
            Node::Atom { .. } => {
                let action = NovelAtomLHS;
                res.push(GraphNode {
                    action,
                    distance: gn.distance + action.cost(),
                    lhs_next: next_node(lhs_next_node, lhs_next_idx.clone()),
                    rhs_next: gn.rhs_next.clone(),
                });
            }
            // Step into this partially/fully novel list.
            Node::List { children, .. } => {
                // TODO: handle unchanged delimiter.
                let action = NovelDelimiterLHS;
                if children.len() == 0 {
                    res.push(GraphNode {
                        action,
                        distance: gn.distance + action.cost(),
                        lhs_next: next_node(lhs_next_node, lhs_next_idx.clone()),
                        rhs_next: gn.rhs_next.clone(),
                    });
                } else {
                    let mut new_lhs_idx = lhs_next_idx.clone();
                    new_lhs_idx.push(0);

                    res.push(GraphNode {
                        action,
                        distance: gn.distance + action.cost(),
                        lhs_next: Some((children[0], new_lhs_idx)),
                        rhs_next: gn.rhs_next.clone(),
                    });
                }
            }
        }
    }

    if let Some((rhs_next_node, rhs_next_idx)) = &gn.rhs_next {
        match rhs_next_node {
            // Step over this novel atom.
            Node::Atom { .. } => {
                let action = NovelAtomRHS;
                res.push(GraphNode {
                    action,
                    distance: gn.distance + action.cost(),
                    lhs_next: gn.lhs_next.clone(),
                    rhs_next: next_node(rhs_next_node, rhs_next_idx.clone()),
                });
            }
            // Step into this partially/fully novel list.
            Node::List { children, .. } => {
                // TODO: handle unchanged delimiter.
                let action = NovelDelimiterRHS;
                if children.len() == 0 {
                    res.push(GraphNode {
                        action,
                        distance: gn.distance + action.cost(),
                        lhs_next: gn.lhs_next.clone(),
                        rhs_next: next_node(rhs_next_node, rhs_next_idx.clone()),
                    });
                } else {
                    let mut new_rhs_idx = rhs_next_idx.clone();
                    new_rhs_idx.push(0);

                    res.push(GraphNode {
                        action,
                        distance: gn.distance + action.cost(),
                        lhs_next: gn.lhs_next.clone(),
                        rhs_next: Some((children[0], new_rhs_idx)),
                    });
                }
            }
        }
    }

    res
}

fn next_node<'a>(
    current: &'a Node<'a>,
    mut current_idx: Vec<usize>,
) -> Option<(&'a Node<'a>, Vec<usize>)> {
    match current.get_parent() {
        Some(parent) => {
            let idx_in_parent = current_idx.last().unwrap();
            match parent {
                Node::List {
                    children: siblings, ..
                } => match siblings.get(idx_in_parent + 1) {
                    Some(node) => {
                        let new_idx = current_idx.last_mut().unwrap();
                        *new_idx += 1;
                        Some((node, current_idx))
                    }
                    None => {
                        current_idx.pop();
                        next_node(parent, current_idx)
                    }
                },
                Node::Atom { .. } => unreachable!(),
            }
        }
        None => None,
    }
}

impl<'a> PartialEq for GraphNode<'a> {
    fn eq(&self, other: &Self) -> bool {
        self.lhs_next_node() == other.lhs_next_node()
            && self.rhs_next_node() == other.rhs_next_node()
    }
}

impl<'a> Hash for GraphNode<'a> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Deliberately ignore distance: we want to find equal nodes
        // regardless of the distance of different paths to them.
        self.lhs_next_node().hash(state);
        self.rhs_next_node().hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::positions::SingleLineSpan;
    use crate::tree_diff::AtomKind;
    use crate::tree_diff::Node::*;

    use std::cell::Cell;
    use typed_arena::Arena;

    #[test]
    fn identical_atoms() {
        let arena = Arena::new();

        let lhs = arena.alloc(Atom {
            parent: Cell::new(None),
            position: vec![SingleLineSpan {
                line: 0.into(),
                start_col: 0,
                end_col: 1,
            }],
            change: Cell::new(None),
            content: "foo".into(),
            kind: AtomKind::Other,
        });

        // Same as LHS, but with a different position.
        let rhs = arena.alloc(Atom {
            parent: Cell::new(None),
            position: vec![SingleLineSpan {
                line: 1.into(),
                start_col: 2,
                end_col: 3,
            }],
            change: Cell::new(None),
            content: "foo".into(),
            kind: AtomKind::Other,
        });

        let start = GraphNode::new(lhs, rhs);
        let route = find_route(start);

        assert_eq!(route.len(), 2);
        let final_node = route.last().unwrap();
        assert_eq!(final_node.distance, 0);
        assert_eq!(final_node.action, UnchangedNode);
    }
}
