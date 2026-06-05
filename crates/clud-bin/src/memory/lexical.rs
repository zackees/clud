use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, Value, FAST, INDEXED, STORED, STRING, TEXT,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::memory::error::MemoryError;
use crate::memory::ids::MemoryId;
use crate::memory::store::Tier;

const WRITER_HEAP_BYTES: usize = 50_000_000;

fn map_tantivy<E: std::fmt::Display>(e: E) -> MemoryError {
    MemoryError::Tantivy(e.to_string())
}

struct SchemaFields {
    id: Field,
    session_id: Field,
    tier: Field,
    content: Field,
}

fn build_schema() -> (Schema, SchemaFields) {
    let mut sb = Schema::builder();
    let id = sb.add_text_field("id", STRING | STORED);
    let session_id = sb.add_text_field("session_id", STRING | STORED);
    let tier = sb.add_u64_field("tier", INDEXED | STORED | FAST);
    let content = sb.add_text_field("content", TEXT);
    let schema = sb.build();
    (
        schema,
        SchemaFields {
            id,
            session_id,
            tier,
            content,
        },
    )
}

pub struct LexicalIndex {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    fields: SchemaFields,
    tier_field_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexicalHit {
    pub id: MemoryId,
    pub bm25: f32,
}

impl LexicalIndex {
    pub fn open_or_create(dir: &Path) -> Result<Self, MemoryError> {
        std::fs::create_dir_all(dir)?;
        let (schema, fields) = build_schema();
        let mmap_dir = tantivy::directory::MmapDirectory::open(dir).map_err(map_tantivy)?;
        let index = Index::open_or_create(mmap_dir, schema).map_err(map_tantivy)?;
        let writer = index.writer(WRITER_HEAP_BYTES).map_err(map_tantivy)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(map_tantivy)?;
        let tier_field_name = index.schema().get_field_name(fields.tier).to_string();
        Ok(Self {
            index,
            writer,
            reader,
            fields,
            tier_field_name,
        })
    }

    pub fn upsert(
        &mut self,
        id: &MemoryId,
        session_id: Option<&str>,
        tier: Tier,
        content: &str,
    ) -> Result<(), MemoryError> {
        // delete-then-add against a single id term so re-indexing the same
        // memory replaces its row instead of producing a duplicate hit.
        let id_term = Term::from_field_text(self.fields.id, id.as_str());
        self.writer.delete_term(id_term);
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.id, id.as_str());
        if let Some(sid) = session_id {
            doc.add_text(self.fields.session_id, sid);
        }
        doc.add_u64(self.fields.tier, tier.as_i64() as u64);
        doc.add_text(self.fields.content, content);
        self.writer.add_document(doc).map_err(map_tantivy)?;
        Ok(())
    }

    pub fn delete(&mut self, id: &MemoryId) -> Result<(), MemoryError> {
        let id_term = Term::from_field_text(self.fields.id, id.as_str());
        self.writer.delete_term(id_term);
        Ok(())
    }

    pub fn commit(&mut self) -> Result<(), MemoryError> {
        self.writer.commit().map_err(map_tantivy)?;
        self.reader.reload().map_err(map_tantivy)?;
        Ok(())
    }

    pub fn search(
        &self,
        query: &str,
        k: usize,
        session_id: Option<&str>,
        tier_floor: Option<Tier>,
    ) -> Result<Vec<LexicalHit>, MemoryError> {
        let searcher = self.reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![self.fields.content]);
        let user_query = parser.parse_query(query).map_err(map_tantivy)?;

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, user_query)];
        if let Some(sid) = session_id {
            let sid_term = Term::from_field_text(self.fields.session_id, sid);
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(sid_term, IndexRecordOption::Basic)),
            ));
        }
        if let Some(floor) = tier_floor {
            let floor_u64 = floor.as_i64() as u64;
            let range = RangeQuery::new_u64_bounds(
                self.tier_field_name.clone(),
                std::ops::Bound::Included(floor_u64),
                std::ops::Bound::Unbounded,
            );
            clauses.push((Occur::Must, Box::new(range)));
        }
        let combined: Box<dyn Query> = Box::new(BooleanQuery::new(clauses));

        let top = searcher
            .search(&combined, &TopDocs::with_limit(k))
            .map_err(map_tantivy)?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr).map_err(map_tantivy)?;
            let id_value = doc
                .get_first(self.fields.id)
                .and_then(|v| v.as_str())
                .ok_or_else(|| MemoryError::Tantivy("doc missing id field".into()))?
                .to_string();
            out.push(LexicalHit {
                id: MemoryId::parse(&id_value)?,
                bm25: score,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ix(tmp: &tempfile::TempDir) -> LexicalIndex {
        LexicalIndex::open_or_create(tmp.path()).unwrap()
    }

    #[test]
    fn upsert_then_search_returns_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let mut idx = ix(&tmp);
        let id = MemoryId::new_v7();
        idx.upsert(&id, None, Tier::Working, "the quick brown fox")
            .unwrap();
        idx.commit().unwrap();
        let hits = idx.search("quick", 10, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id);
        assert!(hits[0].bm25 > 0.0);
    }

    #[test]
    fn delete_removes_from_bm25() {
        let tmp = tempfile::tempdir().unwrap();
        let mut idx = ix(&tmp);
        let id = MemoryId::new_v7();
        idx.upsert(&id, None, Tier::Working, "alpha beta").unwrap();
        idx.commit().unwrap();
        idx.delete(&id).unwrap();
        idx.commit().unwrap();
        let hits = idx.search("alpha", 10, None, None).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_filters_by_session() {
        let tmp = tempfile::tempdir().unwrap();
        let mut idx = ix(&tmp);
        let a = MemoryId::new_v7();
        let b = MemoryId::new_v7();
        idx.upsert(&a, Some("sess-a"), Tier::Working, "shared word zeta")
            .unwrap();
        idx.upsert(&b, Some("sess-b"), Tier::Working, "shared word zeta")
            .unwrap();
        idx.commit().unwrap();
        let hits = idx.search("zeta", 10, Some("sess-a"), None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, a);
    }

    #[test]
    fn search_filters_by_tier_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut idx = ix(&tmp);
        let working = MemoryId::new_v7();
        let semantic = MemoryId::new_v7();
        idx.upsert(&working, None, Tier::Working, "needle haystack")
            .unwrap();
        idx.upsert(&semantic, None, Tier::Semantic, "needle haystack")
            .unwrap();
        idx.commit().unwrap();
        let hits = idx
            .search("needle", 10, None, Some(Tier::Episodic))
            .unwrap();
        assert!(hits.iter().all(|h| h.id != working));
        assert!(hits.iter().any(|h| h.id == semantic));
    }

    #[test]
    fn commit_makes_writes_visible_to_new_reader() {
        let tmp = tempfile::tempdir().unwrap();
        let mut idx = ix(&tmp);
        let id = MemoryId::new_v7();
        idx.upsert(&id, None, Tier::Working, "before commit")
            .unwrap();
        // Before commit, the reader should not see the doc.
        let pre = idx.search("before", 10, None, None).unwrap();
        assert!(pre.is_empty());
        idx.commit().unwrap();
        let post = idx.search("before", 10, None, None).unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0].id, id);
    }
}
