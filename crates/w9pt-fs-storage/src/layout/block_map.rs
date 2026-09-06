//! Bounded traversal and copy-on-write preparation for sparse mapping pages.

use core::{future::Future, pin::Pin};
use std::collections::BTreeMap;

use crate::{
    ContentRepository, FileCryptoContext, FileId, FormatError, LimitError, LimitKind,
    PreparationIdentity, StorageError, StorageLimits, TargetStore,
    format::{BlobRef, BlockMapPage, BranchEntry, LeafEntry, PageRef, page_coverage, page_slot},
};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;
type SummaryFuture<'a, E> = BoxFuture<'a, Result<Option<(u64, u64)>, StorageError<E>>>;
const PAGE_REPRESENTATIONS_PER_REWRITE_LEVEL: usize = 4;
const BTREE_ENTRY_OVERHEAD: usize = 8 * core::mem::size_of::<usize>();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RewriteMode {
    Preflight,
    Store,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MapWork {
    pub(crate) page_reads: u32,
    pub(crate) page_writes: u32,
    pub(crate) peak_working_bytes: usize,
}

pub(crate) struct MapBudget {
    limits: StorageLimits,
    work: MapWork,
    frontier_bytes: usize,
    prepaid_reads: u32,
    prepaid_writes: u32,
}

impl MapBudget {
    pub(crate) fn new(limits: StorageLimits, update_count: usize) -> Result<Self, LimitError> {
        let per_update = core::mem::size_of::<(u64, Option<BlobRef>)>()
            .saturating_add(limits.max_key_bytes())
            .saturating_add(BTREE_ENTRY_OVERHEAD);
        let frontier_bytes = update_count.saturating_mul(per_update);
        let mut budget = Self {
            limits,
            work: MapWork::default(),
            frontier_bytes,
            prepaid_reads: 0,
            prepaid_writes: 0,
        };
        budget.observe_depth(0)?;
        Ok(budget)
    }

    pub(crate) fn charge_read(&mut self) -> Result<(), LimitError> {
        if self.prepaid_reads != 0 {
            self.prepaid_reads -= 1;
            return Ok(());
        }
        let next = self.work.page_reads.saturating_add(1);
        if next > self.limits.max_map_page_reads() {
            return Err(LimitError::new(
                LimitKind::MapPageReads,
                u64::from(next),
                u64::from(self.limits.max_map_page_reads()),
            ));
        }
        self.work.page_reads = next;
        Ok(())
    }

    pub(crate) fn charge_write(&mut self) -> Result<(), LimitError> {
        if self.prepaid_writes != 0 {
            self.prepaid_writes -= 1;
            return Ok(());
        }
        let next = self.work.page_writes.saturating_add(1);
        if next > self.limits.max_map_page_writes() {
            return Err(LimitError::new(
                LimitKind::MapPageWrites,
                u64::from(next),
                u64::from(self.limits.max_map_page_writes()),
            ));
        }
        self.work.page_writes = next;
        Ok(())
    }

    pub(crate) fn reserve_replay(&mut self) -> Result<(), LimitError> {
        let replay_reads = self.work.page_reads;
        let replay_writes = self.work.page_writes;
        // Every actual page write may additionally require one exact reconciliation GET.
        let total_reads = self
            .work
            .page_reads
            .saturating_add(replay_reads)
            .saturating_add(replay_writes);
        if total_reads > self.limits.max_map_page_reads() {
            return Err(LimitError::new(
                LimitKind::MapPageReads,
                u64::from(total_reads),
                u64::from(self.limits.max_map_page_reads()),
            ));
        }
        let total_writes = self.work.page_writes.saturating_add(replay_writes);
        if total_writes > self.limits.max_map_page_writes() {
            return Err(LimitError::new(
                LimitKind::MapPageWrites,
                u64::from(total_writes),
                u64::from(self.limits.max_map_page_writes()),
            ));
        }
        self.work.page_reads = total_reads;
        self.work.page_writes = total_writes;
        self.prepaid_reads = replay_reads;
        self.prepaid_writes = replay_writes;
        Ok(())
    }

    pub(crate) fn observe_depth(&mut self, pages: usize) -> Result<(), LimitError> {
        // A rewrite can retain the decoded base page, a cloned/constructed page,
        // its encoding, and the immutable-PUT copy at the active level. Charging
        // that same conservative maximum for every retained ancestor also covers
        // cursor frames, vector/B-tree capacity, and allocator overhead.
        let page_bytes = self
            .limits
            .max_map_page_bytes()
            .saturating_mul(PAGE_REPRESENTATIONS_PER_REWRITE_LEVEL)
            .saturating_mul(pages);
        let actual = page_bytes.saturating_add(self.frontier_bytes);
        self.work.peak_working_bytes = self.work.peak_working_bytes.max(actual);
        if actual > self.limits.max_map_working_bytes() {
            return Err(LimitError::new(
                LimitKind::MapWorkingBytes,
                u64::try_from(actual).unwrap_or(u64::MAX),
                u64::try_from(self.limits.max_map_working_bytes()).unwrap_or(u64::MAX),
            ));
        }
        Ok(())
    }
}

pub(crate) struct MapCursor<'a, S> {
    repository: &'a ContentRepository<S>,
    context: &'a FileCryptoContext,
    file_id: FileId,
    root: Option<&'a PageRef>,
    path: Vec<(PageRef, BlockMapPage)>,
}

impl<'a, S: TargetStore> MapCursor<'a, S> {
    pub(crate) fn new(
        repository: &'a ContentRepository<S>,
        context: &'a FileCryptoContext,
        file_id: FileId,
        root: Option<&'a PageRef>,
    ) -> Self {
        Self {
            repository,
            context,
            file_id,
            root,
            path: Vec::new(),
        }
    }

    pub(crate) async fn lookup(
        &mut self,
        block_index: u64,
        budget: &mut MapBudget,
    ) -> Result<Option<BlobRef>, StorageError<S::Error>> {
        while self
            .path
            .last()
            .is_some_and(|(reference, _)| !reference.covers(block_index))
        {
            self.path.pop();
        }
        if self.path.is_empty() {
            let Some(root) = self.root.filter(|root| root.covers(block_index)) else {
                return Ok(None);
            };
            let page =
                load_page(self.repository, self.context, self.file_id, root, budget, 1).await?;
            self.path.push((root.clone(), page));
        }

        loop {
            let (_, page) = self.path.last().expect("cursor path initialized");
            if let Some(entries) = page.leaf_entries() {
                let slot = page_slot(0, page.first_block(), block_index)?;
                return Ok(entries
                    .binary_search_by_key(&slot, LeafEntry::slot)
                    .ok()
                    .map(|position| entries[position].blob().clone()));
            }
            let slot = page_slot(page.level(), page.first_block(), block_index)?;
            let entries = page.branch_entries().expect("branch entries");
            let Some(child) = entries
                .binary_search_by_key(&slot, BranchEntry::slot)
                .ok()
                .map(|position| entries[position].child().clone())
            else {
                return Ok(None);
            };
            let depth = self.path.len().saturating_add(1);
            let child_page = load_page(
                self.repository,
                self.context,
                self.file_id,
                &child,
                budget,
                depth,
            )
            .await?;
            self.path.push((child, child_page));
        }
    }
}

async fn load_page<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    file_id: FileId,
    reference: &PageRef,
    budget: &mut MapBudget,
    depth: usize,
) -> Result<BlockMapPage, StorageError<S::Error>> {
    budget.observe_depth(depth)?;
    budget.charge_read()?;
    repository.load_map_page(context, file_id, reference).await
}

pub(crate) async fn retained_summary<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    file_id: FileId,
    root: Option<&PageRef>,
    cutoff: u64,
    budget: &mut MapBudget,
) -> Result<Option<(u64, u64)>, StorageError<S::Error>> {
    let Some(root) = root else {
        return Ok(None);
    };
    retained_summary_ref(repository, context, file_id, root, cutoff, budget, 1).await
}

fn retained_summary_ref<'a, S: TargetStore + 'a>(
    repository: &'a ContentRepository<S>,
    context: &'a FileCryptoContext,
    file_id: FileId,
    reference: &'a PageRef,
    cutoff: u64,
    budget: &'a mut MapBudget,
    depth: usize,
) -> SummaryFuture<'a, S::Error> {
    Box::pin(async move {
        if reference.first_block() >= cutoff {
            return Ok(None);
        }
        let end = reference
            .first_block()
            .checked_add(page_coverage(reference.level())?)
            .ok_or(FormatError::ArithmeticOverflow {
                field: "page range",
            })?;
        if end <= cutoff {
            return Ok(Some((
                reference.materialized_block_count(),
                reference.highest_materialized_block(),
            )));
        }
        let page = load_page(repository, context, file_id, reference, budget, depth).await?;
        if let Some(entries) = page.leaf_entries() {
            let mut count = 0_u64;
            let mut highest = None;
            for entry in entries {
                let index = page.first_block() + u64::from(entry.slot());
                if index < cutoff {
                    count += 1;
                    highest = Some(index);
                }
            }
            return Ok(highest.map(|highest| (count, highest)));
        }
        let mut count = 0_u64;
        let mut highest = None;
        for entry in page.branch_entries().expect("branch") {
            if let Some((child_count, child_highest)) = retained_summary_ref(
                repository,
                context,
                file_id,
                entry.child(),
                cutoff,
                budget,
                depth + 1,
            )
            .await?
            {
                count = count
                    .checked_add(child_count)
                    .ok_or(FormatError::ArithmeticOverflow {
                        field: "retained materialized count",
                    })?;
                highest =
                    Some(highest.map_or(child_highest, |prior: u64| prior.max(child_highest)));
            }
        }
        Ok(highest.map(|highest| (count, highest)))
    })
}

pub(crate) async fn highest_old_excluding<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    file_id: FileId,
    root: Option<&PageRef>,
    updates: &BTreeMap<u64, Option<BlobRef>>,
    budget: &mut MapBudget,
) -> Result<Option<u64>, StorageError<S::Error>> {
    let Some(root) = root else {
        return Ok(None);
    };
    highest_excluding_ref(repository, context, file_id, root, updates, budget, 1).await
}

fn highest_excluding_ref<'a, S: TargetStore + 'a>(
    repository: &'a ContentRepository<S>,
    context: &'a FileCryptoContext,
    file_id: FileId,
    reference: &'a PageRef,
    updates: &'a BTreeMap<u64, Option<BlobRef>>,
    budget: &'a mut MapBudget,
    depth: usize,
) -> BoxFuture<'a, Result<Option<u64>, StorageError<S::Error>>> {
    Box::pin(async move {
        let end = reference
            .first_block()
            .checked_add(page_coverage(reference.level())?)
            .ok_or(FormatError::ArithmeticOverflow {
                field: "page range",
            })?;
        if updates.range(reference.first_block()..end).next().is_none() {
            return Ok(Some(reference.highest_materialized_block()));
        }
        let page = load_page(repository, context, file_id, reference, budget, depth).await?;
        if let Some(entries) = page.leaf_entries() {
            return Ok(entries.iter().rev().find_map(|entry| {
                let index = page.first_block() + u64::from(entry.slot());
                (!updates.contains_key(&index)).then_some(index)
            }));
        }
        for entry in page.branch_entries().expect("branch").iter().rev() {
            if let Some(highest) = highest_excluding_ref(
                repository,
                context,
                file_id,
                entry.child(),
                updates,
                budget,
                depth + 1,
            )
            .await?
            {
                return Ok(Some(highest));
            }
        }
        Ok(None)
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn rewrite_tree<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    file_id: FileId,
    old_root: Option<&PageRef>,
    updates: &BTreeMap<u64, Option<BlobRef>>,
    cutoff: Option<u64>,
    desired_level: u8,
    identity: PreparationIdentity,
    attempt: u32,
    budget: &mut MapBudget,
    mode: RewriteMode,
) -> Result<Option<PageRef>, StorageError<S::Error>> {
    let old = root_at_level(
        repository,
        context,
        file_id,
        old_root,
        desired_level,
        budget,
    )
    .await?;
    rewrite_node(
        repository,
        context,
        file_id,
        desired_level,
        0,
        old.as_ref(),
        updates,
        cutoff,
        identity,
        attempt,
        budget,
        mode,
        1,
    )
    .await
}

async fn root_at_level<S: TargetStore>(
    repository: &ContentRepository<S>,
    context: &FileCryptoContext,
    file_id: FileId,
    root: Option<&PageRef>,
    desired_level: u8,
    budget: &mut MapBudget,
) -> Result<Option<PageRef>, StorageError<S::Error>> {
    let Some(mut current) = root.cloned() else {
        return Ok(None);
    };
    let mut depth = 1;
    while current.level() > desired_level {
        let page = load_page(repository, context, file_id, &current, budget, depth).await?;
        let Some(child) = page
            .branch_entries()
            .and_then(|entries| entries.first())
            .filter(|entry| entry.slot() == 0)
            .map(|entry| entry.child().clone())
        else {
            return Ok(None);
        };
        current = child;
        depth += 1;
    }
    Ok(Some(current))
}

#[allow(clippy::too_many_arguments)]
fn rewrite_node<'a, S: TargetStore + 'a>(
    repository: &'a ContentRepository<S>,
    context: &'a FileCryptoContext,
    file_id: FileId,
    level: u8,
    first_block: u64,
    old: Option<&'a PageRef>,
    updates: &'a BTreeMap<u64, Option<BlobRef>>,
    cutoff: Option<u64>,
    identity: PreparationIdentity,
    attempt: u32,
    budget: &'a mut MapBudget,
    mode: RewriteMode,
    depth: usize,
) -> BoxFuture<'a, Result<Option<PageRef>, StorageError<S::Error>>> {
    Box::pin(async move {
        budget.observe_depth(depth)?;
        let coverage = page_coverage(level)?;
        let end = first_block
            .checked_add(coverage)
            .ok_or(FormatError::ArithmeticOverflow {
                field: "mapping page range",
            })?;
        if cutoff.is_some_and(|cutoff| first_block >= cutoff) {
            return Ok(None);
        }
        let has_updates = updates.range(first_block..end).next().is_some();
        if !has_updates && cutoff.is_none_or(|cutoff| end <= cutoff) {
            if let Some(old) =
                old.filter(|old| old.level() == level && old.first_block() == first_block)
            {
                return Ok(Some(old.clone()));
            }
            if old.is_none() {
                return Ok(None);
            }
        }

        let mut old_page = None;
        if let Some(reference) =
            old.filter(|old| old.level() == level && old.first_block() == first_block)
        {
            old_page =
                Some(load_page(repository, context, file_id, reference, budget, depth).await?);
        }

        let page = if level == 0 {
            let mut entries = BTreeMap::<u8, BlobRef>::new();
            if let Some(page) = &old_page {
                for entry in page
                    .leaf_entries()
                    .ok_or(FormatError::NonCanonical { field: "leaf page" })?
                {
                    entries.insert(entry.slot(), entry.blob().clone());
                }
            }
            if let Some(reference) = old.filter(|old| old.level() < level) {
                let _ = reference;
            }
            if let Some(cutoff) = cutoff {
                entries.retain(|slot, _| first_block + u64::from(*slot) < cutoff);
            }
            for (&index, replacement) in updates.range(first_block..end) {
                let slot = page_slot(0, first_block, index)?;
                match replacement {
                    Some(blob) => {
                        entries.insert(slot, blob.clone());
                    }
                    None => {
                        entries.remove(&slot);
                    }
                }
            }
            if entries.is_empty() {
                return Ok(None);
            }
            BlockMapPage::leaf(
                file_id,
                first_block,
                entries
                    .into_iter()
                    .map(|(slot, blob)| LeafEntry::new(slot, blob))
                    .collect(),
            )
        } else {
            let child_coverage = page_coverage(level - 1)?;
            let mut children = BTreeMap::<u8, PageRef>::new();
            if let Some(page) = &old_page {
                for entry in page.branch_entries().ok_or(FormatError::NonCanonical {
                    field: "branch page",
                })? {
                    children.insert(entry.slot(), entry.child().clone());
                }
            } else if let Some(reference) = old.filter(|old| old.level() < level) {
                children.insert(0, reference.clone());
            }

            for slot in 0..crate::BLOCK_MAP_FANOUT {
                let slot = u8::try_from(slot).expect("128-way fanout");
                let child_first = first_block
                    .checked_add(u64::from(slot) * child_coverage)
                    .ok_or(FormatError::ArithmeticOverflow {
                        field: "child page range",
                    })?;
                let child_end = child_first.checked_add(child_coverage).ok_or(
                    FormatError::ArithmeticOverflow {
                        field: "child page range",
                    },
                )?;
                let affected = updates.range(child_first..child_end).next().is_some()
                    || cutoff.is_some_and(|cutoff| cutoff > child_first && cutoff < child_end)
                    || children
                        .get(&slot)
                        .is_some_and(|child| child.level() != level - 1);
                if cutoff.is_some_and(|cutoff| child_first >= cutoff) {
                    children.remove(&slot);
                    continue;
                }
                if !affected {
                    continue;
                }
                let replacement = rewrite_node(
                    repository,
                    context,
                    file_id,
                    level - 1,
                    child_first,
                    children.get(&slot),
                    updates,
                    cutoff,
                    identity,
                    attempt,
                    budget,
                    mode,
                    depth + 1,
                )
                .await?;
                match replacement {
                    Some(child) => {
                        children.insert(slot, child);
                    }
                    None => {
                        children.remove(&slot);
                    }
                }
            }
            if children.is_empty() {
                return Ok(None);
            }
            BlockMapPage::branch(
                file_id,
                level,
                first_block,
                children
                    .into_iter()
                    .map(|(slot, child)| BranchEntry::new(slot, child))
                    .collect(),
            )
        };

        if old_page.as_ref() == Some(&page) {
            return Ok(old.cloned());
        }
        budget.charge_write()?;
        match mode {
            RewriteMode::Preflight => repository
                .expected_map_page(context, &page, identity, attempt)
                .map(|(reference, _)| Some(reference)),
            RewriteMode::Store => repository
                .store_map_page(context, &page, identity, attempt)
                .await
                .map(Some),
        }
    })
}
