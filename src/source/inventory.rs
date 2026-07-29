use super::Commit;
use super::Result;
use super::model::{SourceEntryKind, SourceEntryMeta, SourceInventory, SourcePath};
use super::snapshot::SnapshotId;

pub(super) fn snapshot_commit(snapshot: &SnapshotId) -> &Commit {
    snapshot.commit()
}

pub(super) fn populate_inventory<I>(leaves: I) -> Result<SourceInventory>
where
    I: IntoIterator<Item = (String, SourceEntryKind)>,
{
    let mut entries = leaves
        .into_iter()
        .map(|(path, kind)| SourcePath::new(&path).map(|path| SourceEntryMeta { path, kind }))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort();
    Ok(SourceInventory { entries })
}
