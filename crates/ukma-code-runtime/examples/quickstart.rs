use ukma_code_runtime::{
    EventOutbox, GraphStore, JsonFileAdapter, ReadContext, SymbolRecord, WriteContext,
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!("ukma-quickstart-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let store = JsonFileAdapter::open(&path).map_err(|e| format!("{e:?}"))?;
    let ctx = WriteContext {
        tenant_id: "demo".into(),
        project_id: "project".into(),
        idempotency_key: "seed".into(),
        expected_etag: None,
        can_index: true,
    };
    let record = SymbolRecord {
        id: "symbol:demo".into(),
        tenant_id: "demo".into(),
        project_id: "project".into(),
        repo_id: "example".into(),
        revision: "r1".into(),
        name: "hello".into(),
        kind: "function".into(),
        etag: 0,
    };
    let result = store
        .upsert_symbol(&ctx, record)
        .map_err(|e| format!("{e:?}"))?;
    let read = ReadContext {
        tenant_id: "demo".into(),
        project_id: "project".into(),
        can_read: true,
    };
    let loaded = store
        .get_symbol(&read, &result.id)
        .map_err(|e| format!("{e:?}"))?;
    println!(
        "{} etag={} pending={}",
        loaded.name,
        loaded.etag,
        store.pending().map_err(|e| format!("{e:?}"))?.len()
    );
    std::fs::remove_file(path)?;
    Ok(())
}
