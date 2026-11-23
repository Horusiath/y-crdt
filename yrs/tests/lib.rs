mod array;
mod compatibility;
mod edit_traces;
mod formatter;
mod map;
mod text;
#[cfg(feature = "weak")]
mod weak;
mod xml;

use yrs::node::Path;
use yrs::test_utils::on_commit;
use yrs::{Doc, NodeID, Reporter, Transaction};

/// Pushes paths of nodes modified within `root` into `paths`, shortest first, if there were any.
fn deep_paths<'a>(
    root: impl Into<NodeID> + 'a,
    paths: &'a mut Vec<Vec<Path>>,
) -> impl Reporter + 'a {
    let root = root.into();
    on_commit(move |t: &Transaction<&Doc>| {
        let mut path: Vec<Path> = t.modified_within(root.clone()).map(|n| n.path()).collect();
        path.sort_by_key(|p| p.len());
        if !path.is_empty() {
            paths.push(path);
        }
    })
}
