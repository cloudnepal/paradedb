// Copyright (c) 2023-2024 Retake, Inc.
//
// This file is part of ParadeDB - Postgres for Search and Analytics
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

use crate::postgres::storage::SKIPLIST_FREQ;
use pgrx::*;
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Formatter};
use std::hash::Hash;
use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};
use std::slice::from_raw_parts;
use std::str::FromStr;
use tantivy::index::{SegmentComponent, SegmentId};
use tantivy::Opstamp;

pub const MERGE_LOCK: pg_sys::BlockNumber = 0;
pub const CLEANUP_LOCK: pg_sys::BlockNumber = 1;
pub const SCHEMA_START: pg_sys::BlockNumber = 2;
pub const SETTINGS_START: pg_sys::BlockNumber = 4;
pub const SEGMENT_METAS_START: pg_sys::BlockNumber = 6;

// ---------------------------------------------------------
// BM25 page special data
// ---------------------------------------------------------

// Struct for all page's LP_SPECIAL data
#[derive(Debug)]
pub struct BM25PageSpecialData {
    pub next_blockno: pg_sys::BlockNumber,
    pub xmax: pg_sys::TransactionId,
}

// ---------------------------------------------------------
// Merge lock
// ---------------------------------------------------------

#[derive(Debug)]
pub struct MergeLockData {
    pub last_merge: pg_sys::TransactionId,
}

// ---------------------------------------------------------
// Linked lists
// ---------------------------------------------------------

/// Struct held in the first buffer of every linked list's content area
#[derive(Copy, Clone)]
#[repr(C, packed)]
pub struct LinkedListData {
    pub inner: Inner,

    /// contains every [`SKIPLIST_FREQ`]th BlockNumber in the list, and
    /// element zero is always the first block number
    pub skip_list: [pg_sys::BlockNumber; {
        (bm25_max_free_space() - size_of::<Inner>()) / size_of::<pg_sys::BlockNumber>()
    }],
}

#[derive(Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct Inner {
    /// Indicates the last BlockNumber of the linked list.
    pub last_blockno: pg_sys::BlockNumber,

    /// Counts the total number of data pages in the linked list (excludes the header page)
    pub npages: u32,
}

impl Debug for LinkedListData {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkedListData")
            .field("inner", &{ self.inner })
            .field("skip_list", &{ self.skip_list })
            .finish()
    }
}

/// Every linked list must implement this trait
pub trait LinkedList {
    // Required methods
    fn get_header_blockno(&self) -> pg_sys::BlockNumber;

    // Provided methods
    fn get_start_blockno(&self) -> pg_sys::BlockNumber {
        let metadata = unsafe { self.get_linked_list_data() };
        let start_blockno = metadata.skip_list[0];
        assert!(start_blockno != 0);
        assert!(start_blockno != pg_sys::InvalidBlockNumber);
        start_blockno
    }

    fn get_last_blockno(&self) -> pg_sys::BlockNumber {
        let metadata = unsafe { self.get_linked_list_data() };
        let last_blockno = metadata.inner.last_blockno;
        assert!(last_blockno != 0);
        assert!(last_blockno != pg_sys::InvalidBlockNumber);
        last_blockno
    }

    fn npages(&self) -> u32 {
        let metadata = unsafe { self.get_linked_list_data() };
        metadata.inner.npages - 1
    }

    fn nearest_block_by_ord(&self, ord: usize) -> (pg_sys::BlockNumber, usize) {
        let index = ord / SKIPLIST_FREQ;
        let metadata = unsafe { self.get_linked_list_data() };
        (metadata.skip_list[index], index * SKIPLIST_FREQ)
    }

    unsafe fn get_linked_list_data(&self) -> LinkedListData;
}

// ---------------------------------------------------------
// Linked list entry structs
// ---------------------------------------------------------

/// Metadata for tracking where to find a file
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FileEntry {
    pub staring_block: pg_sys::BlockNumber,
    pub total_bytes: usize,
}

/// Metadata for tracking where to find a ".del" file
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct DeleteEntry {
    pub file_entry: FileEntry,
    pub num_deleted_docs: u32,
    pub opstamp: Opstamp,
}

/// Metadata for tracking alive segments
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SegmentMetaEntry {
    pub segment_id: SegmentId,
    pub max_doc: u32,
    pub opstamp: tantivy::Opstamp,
    pub xmin: pg_sys::TransactionId,
    pub xmax: pg_sys::TransactionId,

    pub postings: Option<FileEntry>,
    pub positions: Option<FileEntry>,
    pub fast_fields: Option<FileEntry>,
    pub field_norms: Option<FileEntry>,
    pub terms: Option<FileEntry>,
    pub store: Option<FileEntry>,
    pub temp_store: Option<FileEntry>,
    pub delete: Option<DeleteEntry>,
}

impl Default for SegmentMetaEntry {
    fn default() -> Self {
        Self {
            segment_id: SegmentId::from_uuid_string(&uuid::Uuid::default().to_string()).unwrap(),
            max_doc: Default::default(),
            opstamp: Default::default(),
            xmin: pg_sys::InvalidTransactionId,
            xmax: pg_sys::InvalidTransactionId,
            postings: None,
            positions: None,
            fast_fields: None,
            field_norms: None,
            terms: None,
            store: None,
            temp_store: None,
            delete: None,
        }
    }
}

// ---------------------------------------------------------
// Linked list entry <-> PgItem
// ---------------------------------------------------------

/// Wrapper for pg_sys::Item that also stores its size
pub struct PgItem(pub pg_sys::Item, pub pg_sys::Size);

impl From<SegmentMetaEntry> for PgItem {
    fn from(val: SegmentMetaEntry) -> Self {
        let bytes: Vec<u8> =
            bincode::serialize(&val).expect("expected to serialize valid SegmentMetaEntry");
        let pg_bytes = unsafe { pg_sys::palloc(bytes.len()) as *mut u8 };
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pg_bytes, bytes.len());
        }
        PgItem(pg_bytes as pg_sys::Item, bytes.len() as pg_sys::Size)
    }
}

impl From<PgItem> for SegmentMetaEntry {
    fn from(pg_item: PgItem) -> Self {
        let PgItem(item, size) = pg_item;
        let decoded: SegmentMetaEntry = unsafe {
            bincode::deserialize(from_raw_parts(item as *const u8, size))
                .expect("expected to deserialize valid SegmentMetaEntry")
        };
        decoded
    }
}

impl SegmentMetaEntry {
    pub fn get_file_entry(&self, segment_component: SegmentComponent) -> Option<FileEntry> {
        match segment_component {
            SegmentComponent::Postings => self.postings,
            SegmentComponent::Positions => self.positions,
            SegmentComponent::FastFields => self.fast_fields,
            SegmentComponent::FieldNorms => self.field_norms,
            SegmentComponent::Terms => self.terms,
            SegmentComponent::Store => self.store,
            SegmentComponent::TempStore => self.temp_store,
            SegmentComponent::Delete => self.delete.map(|entry| entry.file_entry),
        }
    }

    pub fn get_component_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::with_capacity(8);

        let uuid = self.segment_id.uuid_string();
        if self.postings.is_some() {
            paths.push(PathBuf::from(format!(
                "{}.{}",
                uuid,
                SegmentComponent::Postings
            )));
        }
        if self.positions.is_some() {
            paths.push(PathBuf::from(format!(
                "{}.{}",
                uuid,
                SegmentComponent::Positions
            )));
        }
        if self.fast_fields.is_some() {
            paths.push(PathBuf::from(format!(
                "{}.{}",
                uuid,
                SegmentComponent::FastFields
            )));
        }
        if self.field_norms.is_some() {
            paths.push(PathBuf::from(format!(
                "{}.{}",
                uuid,
                SegmentComponent::FieldNorms
            )));
        }
        if self.terms.is_some() {
            paths.push(PathBuf::from(format!(
                "{}.{}",
                uuid,
                SegmentComponent::Terms
            )));
        }
        if self.store.is_some() {
            paths.push(PathBuf::from(format!(
                "{}.{}",
                uuid,
                SegmentComponent::Store
            )));
        }
        if self.temp_store.is_some() {
            paths.push(PathBuf::from(format!(
                "{}.{}",
                uuid,
                SegmentComponent::TempStore
            )));
        }
        if let Some(entry) = &self.delete {
            paths.push(PathBuf::from(format!(
                "{}.{}.{}",
                uuid,
                entry.opstamp,
                SegmentComponent::Delete
            )));
        }

        paths
    }
}

pub trait SegmentFileDetails {
    fn segment_id(&self) -> Option<SegmentId>;
    fn component_type(&self) -> Option<SegmentComponent>;
    fn opstamp(&self) -> Option<Opstamp>;
}

impl<T: AsRef<Path>> SegmentFileDetails for T {
    fn segment_id(&self) -> Option<SegmentId> {
        let mut parts = self.as_ref().file_name()?.to_str()?.split('.');
        SegmentId::from_uuid_string(parts.next()?).ok()
    }

    fn component_type(&self) -> Option<SegmentComponent> {
        let mut parts = self.as_ref().file_name()?.to_str()?.split('.');
        let _ = parts.next()?; // skip segment id
        let mut extension = parts.next()?;
        if let Some(last) = parts.next() {
            // it has three parts, so the extension is instead the last part
            extension = last;
        }
        SegmentComponent::try_from(extension).ok()
    }

    fn opstamp(&self) -> Option<Opstamp> {
        let mut parts = self.as_ref().file_name()?.to_str()?.split('.');
        let _ = parts.next()?; // skip segment id
        let opstamp = parts.next()?;
        if parts.next().is_some() {
            // there needs to be 3 parts in order for there to be an opstamp in the middle
            return None;
        }

        // if the opstamp doesn't parse, then the Path doesn't have one, and that's okay
        Opstamp::from_str(opstamp).ok()
    }
}

// ---------------------------------------------------------
// Linked list entry MVCC methods
// ---------------------------------------------------------

pub trait MVCCEntry {
    // Required methods
    fn get_xmin(&self) -> pg_sys::TransactionId;
    fn get_xmax(&self) -> pg_sys::TransactionId;
    fn into_frozen(self, should_freeze_xmin: bool, should_freeze_xmax: bool) -> Self;

    // Provided methods
    unsafe fn visible(&self, snapshot: pg_sys::Snapshot) -> bool {
        let xmin = self.get_xmin();
        let xmax = self.get_xmax();
        let xmin_visible = pg_sys::TransactionIdIsCurrentTransactionId(xmin)
            || !pg_sys::XidInMVCCSnapshot(xmin, snapshot);
        let deleted = xmax != pg_sys::InvalidTransactionId
            && (pg_sys::TransactionIdIsCurrentTransactionId(xmax)
                || !pg_sys::XidInMVCCSnapshot(xmax, snapshot));
        xmin_visible && !deleted
    }

    unsafe fn recyclable(
        &self,
        snapshot: pg_sys::Snapshot,
        heap_relation: pg_sys::Relation,
    ) -> bool {
        let xmax = self.get_xmax();
        if xmax == pg_sys::InvalidTransactionId {
            return false;
        }

        if pg_sys::XidInMVCCSnapshot(xmax, snapshot) {
            return false;
        }

        #[cfg(feature = "pg13")]
        {
            pg_sys::TransactionIdPrecedes(xmax, pg_sys::RecentGlobalXmin)
        }

        #[cfg(any(feature = "pg14", feature = "pg15", feature = "pg16", feature = "pg17"))]
        {
            pg_sys::GlobalVisCheckRemovableXid(heap_relation, xmax)
        }
    }

    unsafe fn xmin_needs_freeze(&self, freeze_limit: pg_sys::TransactionId) -> bool {
        let xmin = self.get_xmin();
        pg_sys::TransactionIdIsNormal(xmin) && pg_sys::TransactionIdPrecedes(xmin, freeze_limit)
    }

    unsafe fn xmax_needs_freeze(&self, freeze_limit: pg_sys::TransactionId) -> bool {
        let xmax = self.get_xmax();
        pg_sys::TransactionIdIsNormal(xmax) && pg_sys::TransactionIdPrecedes(xmax, freeze_limit)
    }
}

impl MVCCEntry for SegmentMetaEntry {
    fn get_xmin(&self) -> pg_sys::TransactionId {
        self.xmin
    }
    fn get_xmax(&self) -> pg_sys::TransactionId {
        self.xmax
    }
    fn into_frozen(self, should_freeze_xmin: bool, should_freeze_xmax: bool) -> Self {
        SegmentMetaEntry {
            xmin: if should_freeze_xmin {
                pg_sys::FrozenTransactionId
            } else {
                self.xmin
            },
            xmax: if should_freeze_xmax {
                pg_sys::FrozenTransactionId
            } else {
                self.xmax
            },
            ..self.clone()
        }
    }
}

pub const fn bm25_max_free_space() -> usize {
    unsafe {
        (pg_sys::BLCKSZ as usize)
            - pg_sys::MAXALIGN(size_of::<BM25PageSpecialData>())
            - pg_sys::MAXALIGN(offset_of!(pg_sys::PageHeaderData, pd_linp))
    }
}

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use super::*;

    #[pg_test]
    unsafe fn test_needs_freeze() {
        let freeze_limit = 100;
        let segment = SegmentMetaEntry {
            xmin: 50,
            xmax: 150,
            ..Default::default()
        };

        let xmin_needs_freeze = segment.xmin_needs_freeze(freeze_limit);
        let xmax_needs_freeze = segment.xmax_needs_freeze(freeze_limit);

        assert!(xmin_needs_freeze);
        assert!(!xmax_needs_freeze);

        let frozen_segment = segment
            .clone()
            .into_frozen(xmin_needs_freeze, xmax_needs_freeze);

        assert_eq!(
            frozen_segment,
            SegmentMetaEntry {
                xmin: pg_sys::FrozenTransactionId,
                ..segment.clone()
            }
        );
    }
}