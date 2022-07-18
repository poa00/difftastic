//! Implements Dijkstra's algorithm for shortest path, to find an
//! optimal and readable diff between two ASTs.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
    env,
};

use crate::{
    diff::changes::ChangeMap,
    diff::graph::{neighbours, populate_change_map, Edge, Vertex},
    parse::syntax::Syntax,
};
use bumpalo::Bump;
use itertools::Itertools;
use rustc_hash::FxHashMap;

type PredecessorInfo<'a, 'b> = (u64, &'b Vertex<'a>);

#[derive(Debug)]
pub struct ExceededGraphLimit {}

#[derive(Eq)]
struct OrdByFirst<T>(u64, T);

impl<T> PartialOrd for OrdByFirst<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl<T: Eq> Ord for OrdByFirst<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl<T> PartialEq for OrdByFirst<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

// Admissible, but not consistent (not monotone).
fn estimated_distance_remaining(v: &Vertex) -> u64 {
    let lhs_num_after = match v.lhs_syntax {
        Some(lhs_syntax) => lhs_syntax.num_after() as u64,
        None => 0,
    };
    let rhs_num_after = match v.rhs_syntax {
        Some(rhs_syntax) => rhs_syntax.num_after() as u64,
        None => 0,
    };

    let lhs_num_siblings_after = match v.lhs_syntax {
        Some(lhs_syntax) => lhs_syntax.num_siblings_after() as u64,
        None => 0,
    };
    let rhs_num_siblings_after = match v.rhs_syntax {
        Some(rhs_syntax) => rhs_syntax.num_siblings_after() as u64,
        None => 0,
    };

    let exit_costs = v.num_parents() as u64 * Edge::ExitDelimiterBoth.cost();

    let max_common_this_level = std::cmp::min(lhs_num_siblings_after, rhs_num_siblings_after);
    let min_novel_this_level =
        std::cmp::max(lhs_num_siblings_after, rhs_num_siblings_after) - max_common_this_level;

    // Best case scenario: we match up all of these.
    let max_other_levels = std::cmp::min(
        lhs_num_after - lhs_num_siblings_after,
        rhs_num_after - rhs_num_siblings_after,
    );
    // For the remaining, they must be novel in some form.
    let min_novel_other_levels = std::cmp::max(
        lhs_num_after - lhs_num_siblings_after,
        rhs_num_after - rhs_num_siblings_after,
    ) - max_other_levels;

    (max_common_this_level + max_other_levels)
        * Edge::UnchangedNode {
            depth_difference: 0,
        }
        .cost()
        + (min_novel_this_level + min_novel_other_levels)
            * Edge::NovelAtomLHS { contiguous: true }.cost()
        + exit_costs
}

fn fringe_search(start: Vertex, size_hint: usize) -> Vec<Vertex> {
    let mut threshold: u64 = estimated_distance_remaining(&start);
    let mut threshold_next: u64 = u64::MAX;

    let mut now = VecDeque::new();
    let mut later = VecDeque::new();
    now.push_front((threshold, &start));

    let vertex_arena = Bump::new();
    let mut predecessors: FxHashMap<&Vertex, PredecessorInfo> = FxHashMap::default();
    predecessors.reserve(size_hint);

    let mut neighbour_buf = [
        None, None, None, None, None, None, None, None, None, None, None, None,
    ];

    let end = loop {
        if let Some((distance, current)) = now.pop_front() {
            if current.is_end() {
                break current;
            }

            neighbours(current, &mut neighbour_buf, &vertex_arena);
            for neighbour in &mut neighbour_buf {
                if let Some((edge, next)) = neighbour.take() {
                    let distance_to_next = distance + edge.cost();
                    let found_shorter_route = match predecessors.get(&next) {
                        Some((prev_shortest, _)) => distance_to_next < *prev_shortest,
                        _ => true,
                    };

                    if found_shorter_route {
                        predecessors.insert(next, (distance_to_next, current));

                        let h_next = distance_to_next + estimated_distance_remaining(next);
                        if h_next <= threshold {
                            now.push_back((distance_to_next, next));
                        } else {
                            later.push_back((distance_to_next, next));

                            if h_next < threshold_next {
                                threshold_next = h_next;
                            }
                        }
                    }
                }
            }
        } else {
            if later.is_empty() {
                panic!("Ran out of nodes");
            } else {
                now = later;
                threshold = threshold_next;
                threshold_next = u64::MAX;

                // TODO: consider reusing the original `now` to avoid
                // reallocating?
                later = VecDeque::new();
            }
        }
    };

    debug!(
        "Found predecessors for {} vertices (hashmap key: {} bytes, value: {} bytes), with {} left on now and {} left on later.",
        predecessors.len(),
        std::mem::size_of::<&Vertex>(),
        std::mem::size_of::<PredecessorInfo>(),
        now.len(),
        later.len(),
    );
    let mut current = end;

    let mut vertex_route: Vec<Vertex> = vec![end.clone()];
    while let Some((_, node)) = predecessors.remove(&current) {
        vertex_route.push(node.clone());
        current = node;
    }

    vertex_route.reverse();
    vertex_route
}

/// Return the shortest route from `start` to the end vertex.
fn shortest_vertex_path(
    start: Vertex,
    size_hint: usize,
    graph_limit: usize,
) -> Result<Vec<Vertex>, ExceededGraphLimit> {
    // We want to visit nodes with the shortest distance first, but
    // RadixHeapMap is a max-heap. Ensure nodes are wrapped with
    // Reverse to flip comparisons.
    let mut heap: BinaryHeap<Reverse<OrdByFirst<(u64, &Vertex)>>> = BinaryHeap::new();
    // let mut heap: RadixHeapMap<Reverse<_>, (u64, &Vertex)> = RadixHeapMap::new();

    let vertex_arena = Bump::new();
    let o = OrdByFirst(
        0 + estimated_distance_remaining(&start),
        (0, vertex_arena.alloc(start.clone()) as &Vertex),
    );
    heap.push(Reverse(o));

    // TODO: this grows very big. Consider using IDA* to reduce memory
    // usage.
    let mut predecessors: FxHashMap<&Vertex, PredecessorInfo> = FxHashMap::default();
    predecessors.reserve(size_hint);

    let mut neighbour_buf = [
        None, None, None, None, None, None, None, None, None, None, None, None,
    ];
    let end = loop {
        match heap.pop() {
            Some(Reverse(OrdByFirst(_, (distance, current)))) => {
                if current.is_end() {
                    break current;
                }

                neighbours(current, &mut neighbour_buf, &vertex_arena);
                for neighbour in &mut neighbour_buf {
                    if let Some((edge, next)) = neighbour.take() {
                        let distance_to_next = distance + edge.cost();
                        let found_shorter_route = match predecessors.get(&next) {
                            Some((prev_shortest, _)) => distance_to_next < *prev_shortest,
                            _ => true,
                        };

                        if found_shorter_route {
                            predecessors.insert(next, (distance_to_next, current));

                            let o = OrdByFirst(
                                distance_to_next + estimated_distance_remaining(next),
                                (distance_to_next, next),
                            );
                            heap.push(Reverse(o));
                        }
                    }
                }
                if predecessors.len() > graph_limit {
                    return Err(ExceededGraphLimit {});
                }
            }
            None => panic!("Ran out of graph nodes before reaching end"),
        }
    };

    debug!(
        "Found predecessors for {} vertices (hashmap key: {} bytes, value: {} bytes), with {} left on heap.",
        predecessors.len(),
        std::mem::size_of::<&Vertex>(),
        std::mem::size_of::<PredecessorInfo>(),
        heap.len(),
    );
    let mut current = end;

    let mut vertex_route: Vec<Vertex> = vec![end.clone()];
    while let Some((_, node)) = predecessors.remove(&current) {
        vertex_route.push(node.clone());
        current = node;
    }

    vertex_route.reverse();
    Ok(vertex_route)
}

fn shortest_path_with_edges<'a>(route: &[Vertex<'a>]) -> Vec<(Edge, Vertex<'a>)> {
    let mut prev = route.first().expect("Expected non-empty route");

    let mut cost = 0;
    let mut res = vec![];
    for vertex in route.iter().skip(1) {
        let edge = edge_between(prev, vertex);
        res.push((edge, prev.clone()));
        cost += edge.cost();

        prev = vertex;
    }
    debug!("Found a path of {} with cost {}.", route.len(), cost);

    res
}

/// Return the shortest route from the `start` to the end vertex.
///
/// The vec returned does not return the very last vertex. This is
/// necessary because a route of N vertices only has N-1 edges.
fn shortest_path(
    start: Vertex,
    size_hint: usize,
    _graph_limit: usize,
) -> Result<Vec<(Edge, Vertex)>, ExceededGraphLimit> {
    let vertex_path = fringe_search(start, size_hint);
    Ok(shortest_path_with_edges(&vertex_path))
}

fn edge_between<'a>(before: &Vertex<'a>, after: &Vertex<'a>) -> Edge {
    let mut neighbour_buf = [
        None, None, None, None, None, None, None, None, None, None, None, None,
    ];

    let vertex_arena = Bump::new();
    neighbours(before, &mut neighbour_buf, &vertex_arena);

    let mut shortest_edge: Option<Edge> = None;
    for neighbour in &mut neighbour_buf {
        if let Some((edge, next)) = neighbour.take() {
            // If there are multiple edges that can take us to `next`,
            // prefer the shortest.
            if next == after {
                let is_shorter = match shortest_edge {
                    Some(prev_edge) => edge.cost() < prev_edge.cost(),
                    None => true,
                };

                if is_shorter {
                    shortest_edge = Some(edge);
                }
            }
        }
    }

    if let Some(edge) = shortest_edge {
        return edge;
    }

    panic!(
        "Expected a route between the two vertices {:#?} and {:#?}",
        before, after
    );
}

/// What is the total number of AST nodes?
fn node_count(root: Option<&Syntax>) -> u32 {
    let mut node = root;
    let mut count = 0;
    while let Some(current_node) = node {
        let current_count = match current_node {
            Syntax::List {
                num_descendants, ..
            } => *num_descendants,
            Syntax::Atom { .. } => 1,
        };
        count += current_count;

        node = current_node.next_sibling();
    }

    count
}

/// How many top-level AST nodes do we have?
fn tree_count(root: Option<&Syntax>) -> u32 {
    let mut node = root;
    let mut count = 0;
    while let Some(current_node) = node {
        count += 1;
        node = current_node.next_sibling();
    }

    count
}

pub fn mark_syntax<'a>(
    lhs_syntax: Option<&'a Syntax<'a>>,
    rhs_syntax: Option<&'a Syntax<'a>>,
    change_map: &mut ChangeMap<'a>,
    graph_limit: usize,
) -> Result<(), ExceededGraphLimit> {
    let lhs_node_count = node_count(lhs_syntax) as usize;
    let rhs_node_count = node_count(rhs_syntax) as usize;
    info!(
        "LHS nodes: {} ({} toplevel), RHS nodes: {} ({} toplevel)",
        lhs_node_count,
        tree_count(lhs_syntax),
        rhs_node_count,
        tree_count(rhs_syntax),
    );

    // When there are a large number of changes, we end up building a
    // graph whose size is roughly quadratic. Use this as a size hint,
    // so we don't spend too much time re-hashing and expanding the
    // predecessors hashmap.
    let size_hint = lhs_node_count * rhs_node_count;

    let start = Vertex::new(lhs_syntax, rhs_syntax);
    let route = shortest_path(start, size_hint, graph_limit)?;

    let print_length = if env::var("DFT_VERBOSE").is_ok() {
        50
    } else {
        5
    };
    debug!(
        "Initial {} items on path: {:#?}",
        print_length,
        route
            .iter()
            .map(|x| {
                format!(
                    "{:20} {:20} --- {:3} {:?}",
                    x.1.lhs_syntax
                        .map_or_else(|| "None".into(), Syntax::dbg_content),
                    x.1.rhs_syntax
                        .map_or_else(|| "None".into(), Syntax::dbg_content),
                    x.0.cost(),
                    x.0,
                )
            })
            .take(print_length)
            .collect_vec()
    );

    populate_change_map(&route, change_map);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::changes::ChangeKind,
        diff::graph::Edge::*,
        options::DEFAULT_GRAPH_LIMIT,
        positions::SingleLineSpan,
        syntax::{init_all_info, AtomKind},
    };

    use itertools::Itertools;
    use typed_arena::Arena;

    fn pos_helper(line: u32) -> Vec<SingleLineSpan> {
        vec![SingleLineSpan {
            line: line.into(),
            start_col: 0,
            end_col: 1,
        }]
    }

    fn col_helper(line: u32, col: u32) -> Vec<SingleLineSpan> {
        vec![SingleLineSpan {
            line: line.into(),
            start_col: col,
            end_col: col + 1,
        }]
    }

    #[test]
    fn identical_atoms() {
        let arena = Arena::new();

        let lhs = Syntax::new_atom(&arena, pos_helper(0), "foo", AtomKind::Normal);
        // Same content as LHS.
        let rhs = Syntax::new_atom(&arena, pos_helper(0), "foo", AtomKind::Normal);
        init_all_info(&[lhs], &[rhs]);

        let start = Vertex::new(Some(lhs), Some(rhs));
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![UnchangedNode {
                depth_difference: 0
            }]
        );
    }

    #[test]
    fn extra_atom_lhs() {
        let arena = Arena::new();

        let lhs = vec![Syntax::new_list(
            &arena,
            "[",
            pos_helper(0),
            vec![Syntax::new_atom(
                &arena,
                pos_helper(1),
                "foo",
                AtomKind::Normal,
            )],
            "]",
            pos_helper(2),
        )];

        let rhs = vec![Syntax::new_list(
            &arena,
            "[",
            pos_helper(0),
            vec![],
            "]",
            pos_helper(2),
        )];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![
                EnterUnchangedDelimiter {
                    depth_difference: 0
                },
                NovelAtomLHS { contiguous: false },
                ExitDelimiterBoth,
            ]
        );
    }

    #[test]
    fn repeated_atoms() {
        let arena = Arena::new();

        let lhs = vec![Syntax::new_list(
            &arena,
            "[",
            pos_helper(0),
            vec![],
            "]",
            pos_helper(2),
        )];

        let rhs = vec![Syntax::new_list(
            &arena,
            "[",
            pos_helper(0),
            vec![
                Syntax::new_atom(&arena, pos_helper(1), "foo", AtomKind::Normal),
                Syntax::new_atom(&arena, pos_helper(2), "foo", AtomKind::Normal),
            ],
            "]",
            pos_helper(3),
        )];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![
                EnterUnchangedDelimiter {
                    depth_difference: 0
                },
                NovelAtomRHS { contiguous: false },
                NovelAtomRHS { contiguous: false },
                ExitDelimiterBoth,
            ]
        );
    }

    #[test]
    fn atom_after_empty_list() {
        let arena = Arena::new();

        let lhs = vec![Syntax::new_list(
            &arena,
            "[",
            pos_helper(0),
            vec![
                Syntax::new_list(&arena, "(", pos_helper(1), vec![], ")", pos_helper(2)),
                Syntax::new_atom(&arena, pos_helper(3), "foo", AtomKind::Normal),
            ],
            "]",
            pos_helper(4),
        )];

        let rhs = vec![Syntax::new_list(
            &arena,
            "{",
            pos_helper(0),
            vec![
                Syntax::new_list(&arena, "(", pos_helper(1), vec![], ")", pos_helper(2)),
                Syntax::new_atom(&arena, pos_helper(3), "foo", AtomKind::Normal),
            ],
            "}",
            pos_helper(4),
        )];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![
                EnterNovelDelimiterRHS { contiguous: false },
                EnterNovelDelimiterLHS { contiguous: false },
                UnchangedNode {
                    depth_difference: 0
                },
                UnchangedNode {
                    depth_difference: 0
                },
                ExitDelimiterRHS,
                ExitDelimiterLHS,
            ],
        );
    }

    #[test]
    fn prefer_atoms_same_line() {
        let arena = Arena::new();

        let lhs = vec![
            Syntax::new_atom(&arena, col_helper(1, 0), "foo", AtomKind::Normal),
            Syntax::new_atom(&arena, col_helper(2, 0), "bar", AtomKind::Normal),
            Syntax::new_atom(&arena, col_helper(2, 1), "foo", AtomKind::Normal),
        ];

        let rhs = vec![Syntax::new_atom(
            &arena,
            col_helper(1, 0),
            "foo",
            AtomKind::Normal,
        )];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![
                UnchangedNode {
                    depth_difference: 0
                },
                NovelAtomLHS { contiguous: false },
                NovelAtomLHS { contiguous: true },
            ]
        );
    }

    #[test]
    fn prefer_children_same_line() {
        let arena = Arena::new();

        let lhs = vec![Syntax::new_list(
            &arena,
            "[",
            col_helper(1, 0),
            vec![Syntax::new_atom(
                &arena,
                col_helper(1, 2),
                "1",
                AtomKind::Normal,
            )],
            "]",
            pos_helper(2),
        )];

        let rhs = vec![];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![
                EnterNovelDelimiterLHS { contiguous: false },
                NovelAtomLHS { contiguous: true },
                ExitDelimiterLHS,
            ]
        );
    }

    #[test]
    fn atom_after_novel_list_contiguous() {
        let arena = Arena::new();

        let lhs = vec![
            Syntax::new_list(
                &arena,
                "[",
                col_helper(1, 0),
                vec![Syntax::new_atom(
                    &arena,
                    col_helper(1, 2),
                    "1",
                    AtomKind::Normal,
                )],
                "]",
                col_helper(2, 1),
            ),
            Syntax::new_atom(&arena, col_helper(2, 2), ";", AtomKind::Normal),
        ];

        let rhs = vec![];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![
                EnterNovelDelimiterLHS { contiguous: false },
                NovelAtomLHS { contiguous: true },
                ExitDelimiterLHS,
                NovelAtomLHS { contiguous: true },
            ]
        );
    }

    #[test]
    fn replace_similar_comment() {
        let arena = Arena::new();

        let lhs = vec![Syntax::new_atom(
            &arena,
            pos_helper(1),
            "the quick brown fox",
            AtomKind::Comment,
        )];

        let rhs = vec![Syntax::new_atom(
            &arena,
            pos_helper(1),
            "the quick brown cat",
            AtomKind::Comment,
        )];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![ReplacedComment {
                levenshtein_pct: 84
            }]
        );
    }

    #[test]
    fn replace_very_different_comment() {
        let arena = Arena::new();

        let lhs = vec![Syntax::new_atom(
            &arena,
            pos_helper(1),
            "the quick brown fox",
            AtomKind::Comment,
        )];

        let rhs = vec![Syntax::new_atom(
            &arena,
            pos_helper(1),
            "foo bar",
            AtomKind::Comment,
        )];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![ReplacedComment {
                levenshtein_pct: 11
            }]
        );
    }

    #[test]
    fn replace_comment_prefer_most_similar() {
        let arena = Arena::new();

        let lhs = vec![
            Syntax::new_atom(
                &arena,
                pos_helper(1),
                "the quick brown fox",
                AtomKind::Comment,
            ),
            Syntax::new_atom(
                &arena,
                pos_helper(2),
                "the quick brown thing",
                AtomKind::Comment,
            ),
        ];

        let rhs = vec![Syntax::new_atom(
            &arena,
            pos_helper(1),
            "the quick brown fox.",
            AtomKind::Comment,
        )];
        init_all_info(&lhs, &rhs);

        let start = Vertex::new(lhs.get(0).copied(), rhs.get(0).copied());
        let route = shortest_path(start, 0, DEFAULT_GRAPH_LIMIT).unwrap();

        let actions = route.iter().map(|(action, _)| *action).collect_vec();
        assert_eq!(
            actions,
            vec![
                ReplacedComment {
                    levenshtein_pct: 95
                },
                NovelAtomLHS { contiguous: false }
            ]
        );
    }

    #[test]
    fn mark_syntax_equal_atoms() {
        let arena = Arena::new();
        let lhs = Syntax::new_atom(&arena, pos_helper(1), "foo", AtomKind::Normal);
        let rhs = Syntax::new_atom(&arena, pos_helper(1), "foo", AtomKind::Normal);
        init_all_info(&[lhs], &[rhs]);

        let mut change_map = ChangeMap::default();
        mark_syntax(Some(lhs), Some(rhs), &mut change_map, DEFAULT_GRAPH_LIMIT).unwrap();

        assert_eq!(change_map.get(lhs), Some(ChangeKind::Unchanged(rhs)));
        assert_eq!(change_map.get(rhs), Some(ChangeKind::Unchanged(lhs)));
    }

    #[test]
    fn mark_syntax_different_atoms() {
        let arena = Arena::new();
        let lhs = Syntax::new_atom(&arena, pos_helper(1), "foo", AtomKind::Normal);
        let rhs = Syntax::new_atom(&arena, pos_helper(1), "bar", AtomKind::Normal);
        init_all_info(&[lhs], &[rhs]);

        let mut change_map = ChangeMap::default();
        mark_syntax(Some(lhs), Some(rhs), &mut change_map, DEFAULT_GRAPH_LIMIT).unwrap();
        assert_eq!(change_map.get(lhs), Some(ChangeKind::Novel));
        assert_eq!(change_map.get(rhs), Some(ChangeKind::Novel));
    }
}
