#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, types::{Value as SV, ValueRef}, Connection, ToSql};
use serde_json::{json, Map, Value};
use std::{fs, path::{Path, PathBuf}, sync::Mutex};
use tauri::{Manager, State};
use tauri_plugin_notification::NotificationExt;

struct Db(Mutex<Connection>, PathBuf);
type R<T> = Result<T, String>;
fn e<E: ToString>(x: E) -> String { x.to_string() }
macro_rules! lk { ($d:expr) => { $d.0.lock().map_err(e)? }; }
macro_rules! cn { ($d:expr) => { &*$d.0.lock().map_err(e)? }; }
const TABLES: [&str; 15] = ["workspaces","boards","lists","cards","labels","card_labels","checklist_items","attachments","card_deps","activity","inbox","templates","settings","milestones","filters"];
const CARDS_SQL: &str = "SELECT c.*,
 (SELECT COUNT(*) FROM checklist_items WHERE card_id=c.id) AS ck_total,
 (SELECT COUNT(*) FROM checklist_items WHERE card_id=c.id AND done=1) AS ck_done,
 (SELECT group_concat(lb.color||'|'||lb.name||'|'||lb.id,';') FROM card_labels cl JOIN labels lb ON lb.id=cl.label_id WHERE cl.card_id=c.id) AS labels,
 (SELECT COUNT(*) FROM card_deps d JOIN cards x ON x.id=d.depends_on WHERE d.card_id=c.id AND x.done=0) AS blocked,
 (SELECT COUNT(*) FROM attachments WHERE card_id=c.id) AS att
 FROM cards c JOIN lists l ON l.id=c.list_id WHERE l.board_id=? AND c.archived=0 ORDER BY c.pos";

fn q(c: &Connection, sql: &str, p: &[&dyn ToSql]) -> R<Vec<Value>> {
    let mut s = c.prepare(sql).map_err(e)?;
    let names: Vec<String> = s.column_names().iter().map(|n| n.to_string()).collect();
    let rows = s.query_map(p, |r| {
        let mut m = Map::new();
        for (i, n) in names.iter().enumerate() {
            m.insert(n.clone(), match r.get_ref(i)? {
                ValueRef::Integer(x) => json!(x), ValueRef::Real(x) => json!(x),
                ValueRef::Text(t) => json!(String::from_utf8_lossy(t)), _ => Value::Null });
        }
        Ok(Value::Object(m))
    }).map_err(e)?;
    rows.collect::<Result<_, _>>().map_err(e)
}
fn x(c: &Connection, sql: &str, p: &[&dyn ToSql]) -> R<()> { c.execute(sql, p).map_err(e)?; Ok(()) }
fn clean(s: String, max: usize) -> R<String> {
    let s = s.trim().to_string();
    if s.is_empty() || s.chars().count() > max { Err(format!("Text must be 1-{max} characters")) } else { Ok(s) }
}
fn log(c: &Connection, card: i64, t: &str) { let _ = c.execute("INSERT INTO activity(card_id,text) VALUES(?,?)", params![card, t]); }
fn jv(v: &Value) -> SV { match v {
    Value::Null => SV::Null, Value::Bool(b) => SV::Integer(*b as i64),
    Value::Number(n) => if let Some(i) = n.as_i64() { SV::Integer(i) } else { SV::Real(n.as_f64().unwrap_or(0.0)) },
    Value::String(s) => SV::Text(s.clone()), o => SV::Text(o.to_string()) } }
fn dump(c: &Connection) -> R<Value> {
    let mut m = Map::new();
    for t in TABLES { m.insert(t.to_string(), Value::Array(q(c, &format!("SELECT * FROM {t}"), &[])?)); }
    Ok(Value::Object(m))
}
fn load(c: &Connection, v: &Value) -> R<()> {
    if !v.is_object() { return Err("Not a Project Manager export".into()); }
    c.execute_batch("PRAGMA foreign_keys=OFF;BEGIN;").map_err(e)?;
    let r = (|| -> R<()> {
        for t in TABLES.iter().rev() { c.execute(&format!("DELETE FROM {t}"), []).map_err(e)?; }
        for t in TABLES {
            if let Some(rows) = v.get(t).and_then(|x| x.as_array()) {
                for row in rows {
                    let o = row.as_object().ok_or("bad row")?;
                    let keys: Vec<&String> = o.keys().collect();
                    if keys.is_empty() || keys.iter().any(|k| !k.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')) { return Err("bad column name".into()); }
                    let sql = format!("INSERT INTO {t}({}) VALUES({})", keys.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(","), vec!["?"; keys.len()].join(","));
                    let vals: Vec<SV> = keys.iter().map(|k| jv(&o[*k])).collect();
                    c.execute(&sql, rusqlite::params_from_iter(vals)).map_err(e)?;
                }
            }
        }
        Ok(())
    })();
    match r {
        Ok(_) => c.execute_batch("COMMIT;PRAGMA foreign_keys=ON;").map_err(e),
        Err(m) => { let _ = c.execute_batch("ROLLBACK;PRAGMA foreign_keys=ON;"); Err(m) }
    }
}
fn make_backup(c: &Connection, dir: &Path, tag: &str) -> R<PathBuf> {
    let d = dir.join("backups"); fs::create_dir_all(&d).map_err(e)?;
    let ts: String = c.query_row("SELECT strftime('%Y%m%d-%H%M%S','now')", [], |r| r.get(0)).map_err(e)?;
    let f = d.join(format!("backup-{ts}{tag}.db"));
    c.execute("VACUUM INTO ?", [f.to_string_lossy().to_string()]).map_err(e)?;
    Ok(f)
}
fn backup_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir.join("backups")).map(|r| r.filter_map(|x| x.ok()).map(|x| x.file_name().to_string_lossy().to_string()).filter(|n| n.starts_with("backup-") && n.ends_with(".db")).collect()).unwrap_or_default();
    v.sort(); v.reverse(); v
}
fn open_it(p: &Path) -> R<()> {
    #[cfg(target_os = "windows")] let mut cmd = std::process::Command::new("explorer");
    #[cfg(target_os = "macos")] let mut cmd = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))] let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(p).spawn().map_err(e)?; Ok(())
}
fn new_board(c: &Connection, ws: i64, name: &str, bg: &str, lists: &str) -> R<i64> {
    x(c, "INSERT INTO boards(workspace_id,name,bg) VALUES(?,?,?)", params![ws, name, bg])?;
    let id = c.last_insert_rowid();
    let ls: Vec<&str> = if lists.is_empty() { vec!["Backlog", "In progress", "Done"] } else { lists.split(',').collect() };
    for (i, n) in ls.iter().enumerate() { x(c, "INSERT INTO lists(board_id,name,pos) VALUES(?,?,?)", params![id, n.trim(), i as f64 + 1.0])?; }
    for (n, col) in [("Bug","#f87171"),("Idea","#fbbf24"),("Urgent","#fb923c"),("Research","#a78bfa"),("Content","#34d399"),("Design","#f472b6")] {
        x(c, "INSERT INTO labels(board_id,name,color) VALUES(?,?,?)", params![id, n, col])?; }
    Ok(id)
}

#[tauri::command] fn workspaces(db: State<Db>) -> R<Vec<Value>> { q(cn!(db), "SELECT * FROM workspaces ORDER BY id", &[]) }
#[tauri::command] fn add_workspace(db: State<Db>, name: String, color: String, icon: String) -> R<()> {
    x(cn!(db), "INSERT INTO workspaces(name,color,icon) VALUES(?,?,?)", params![clean(name, 60)?, color, icon.chars().take(4).collect::<String>()]) }
#[tauri::command] fn edit_workspace(db: State<Db>, id: i64, name: String, color: String, icon: String) -> R<()> {
    x(cn!(db), "UPDATE workspaces SET name=?,color=?,icon=? WHERE id=?", params![clean(name, 60)?, color, icon.chars().take(4).collect::<String>(), id]) }
#[tauri::command] fn del_workspace(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM workspaces WHERE id=?", params![id]) }
#[tauri::command] fn boards(db: State<Db>, ws: i64) -> R<Vec<Value>> { q(cn!(db), "SELECT * FROM boards WHERE workspace_id=? ORDER BY id", &[&ws]) }
#[tauri::command] fn add_board(db: State<Db>, ws: i64, name: String, bg: String) -> R<i64> { new_board(cn!(db), ws, &clean(name, 80)?, &bg, "") }
#[tauri::command] fn edit_board(db: State<Db>, id: i64, name: String, bg: String) -> R<()> { x(cn!(db), "UPDATE boards SET name=?,bg=? WHERE id=?", params![clean(name, 80)?, bg, id]) }
#[tauri::command] fn dup_board(db: State<Db>, id: i64) -> R<i64> {
    let c = lk!(db);
    x(&c, "INSERT INTO boards(workspace_id,name,bg) SELECT workspace_id,name||' (copy)',bg FROM boards WHERE id=?", params![id])?;
    let n = c.last_insert_rowid();
    x(&c, "INSERT INTO lists(board_id,name,pos) SELECT ?,name,pos FROM lists WHERE board_id=?", params![n, id])?;
    x(&c, "INSERT INTO labels(board_id,name,color) SELECT ?,name,color FROM labels WHERE board_id=?", params![n, id])?;
    Ok(n)
}
#[tauri::command] fn del_board(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM boards WHERE id=?", params![id]) }
#[tauri::command] fn board(db: State<Db>, id: i64) -> R<Value> {
    let c = lk!(db);
    Ok(json!({
        "board": q(&c, "SELECT * FROM boards WHERE id=?", &[&id])?.into_iter().next(),
        "lists": q(&c, "SELECT * FROM lists WHERE board_id=? ORDER BY pos", &[&id])?,
        "cards": q(&c, CARDS_SQL, &[&id])?,
        "labels": q(&c, "SELECT * FROM labels WHERE board_id=? ORDER BY id", &[&id])?
    }))
}
#[tauri::command] fn add_list(db: State<Db>, board: i64, name: String) -> R<()> {
    x(cn!(db), "INSERT INTO lists(board_id,name,pos) VALUES(?,?,COALESCE((SELECT MAX(pos)+1 FROM lists WHERE board_id=?),1))", params![board, clean(name, 80)?, board]) }
#[tauri::command] fn rename_list(db: State<Db>, id: i64, name: String) -> R<()> { x(cn!(db), "UPDATE lists SET name=? WHERE id=?", params![clean(name, 80)?, id]) }
#[tauri::command] fn move_list(db: State<Db>, id: i64, dir: i64) -> R<()> {
    let c = lk!(db);
    let (b, p): (i64, f64) = c.query_row("SELECT board_id,pos FROM lists WHERE id=?", [id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
    let sql = if dir < 0 { "SELECT id,pos FROM lists WHERE board_id=? AND pos<? ORDER BY pos DESC LIMIT 1" } else { "SELECT id,pos FROM lists WHERE board_id=? AND pos>? ORDER BY pos ASC LIMIT 1" };
    let nb: Option<(i64, f64)> = c.query_row(sql, params![b, p], |r| Ok((r.get(0)?, r.get(1)?))).ok();
    if let Some((oid, op)) = nb { x(&c, "UPDATE lists SET pos=? WHERE id=?", params![op, id])?; x(&c, "UPDATE lists SET pos=? WHERE id=?", params![p, oid])?; }
    Ok(())
}
#[tauri::command] fn del_list(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM lists WHERE id=?", params![id]) }
#[tauri::command] fn add_card(db: State<Db>, list: i64, title: String) -> R<()> {
    let c = lk!(db); let t = clean(title, 300)?;
    x(&c, "INSERT INTO cards(list_id,title,pos) VALUES(?,?,COALESCE((SELECT MAX(pos)+1 FROM cards WHERE list_id=?),1))", params![list, t, list])?;
    log(&c, c.last_insert_rowid(), "Created"); Ok(())
}
#[tauri::command] fn move_card(db: State<Db>, id: i64, list: i64, pos: f64) -> R<()> {
    let c = lk!(db);
    x(&c, "UPDATE cards SET list_id=?,pos=?,updated=CURRENT_TIMESTAMP WHERE id=?", params![list, pos, id])?; log(&c, id, "Moved"); Ok(())
}
#[tauri::command] fn update_card(db: State<Db>, id: i64, title: String, description: String, due: Option<String>, start: Option<String>, priority: i64, recur: String, fields: String, est: i64) -> R<()> {
    if !["", "daily", "weekly", "monthly"].contains(&recur.as_str()) { return Err("Bad recurrence".into()); }
    let c = lk!(db);
    x(&c, "UPDATE cards SET title=?,description=?,due=?,start=?,priority=?,recur=?,fields=?,est=?,updated=CURRENT_TIMESTAMP WHERE id=?",
        params![clean(title, 300)?, description, due.filter(|d| !d.is_empty()), start.filter(|d| !d.is_empty()), priority.clamp(0, 3), recur, fields, est.max(0), id])
}
#[tauri::command] fn toggle_done(db: State<Db>, id: i64) -> R<()> {
    let c = lk!(db);
    let (recur, done): (String, i64) = c.query_row("SELECT recur,done FROM cards WHERE id=?", [id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
    let blocked: i64 = c.query_row("SELECT COUNT(*) FROM card_deps d JOIN cards x ON x.id=d.depends_on WHERE d.card_id=? AND x.done=0", [id], |r| r.get(0)).map_err(e)?;
    if blocked > 0 && done == 0 { return Err("Blocked: finish its dependencies first".into()); }
    if done == 0 && !recur.is_empty() {
        let m = match recur.as_str() { "weekly" => "+7 days", "monthly" => "+1 month", _ => "+1 day" };
        x(&c, "UPDATE cards SET due=date(max(COALESCE(due,date('now')),date('now')),?),updated=CURRENT_TIMESTAMP WHERE id=?", params![m, id])?;
        x(&c, "UPDATE checklist_items SET done=0 WHERE card_id=?", params![id])?; log(&c, id, "Completed - rescheduled (recurring)");
    } else {
        x(&c, "UPDATE cards SET done=1-done,updated=CURRENT_TIMESTAMP WHERE id=?", params![id])?; log(&c, id, if done == 0 { "Marked done" } else { "Reopened" });
    }
    Ok(())
}
#[tauri::command] fn dup_card(db: State<Db>, id: i64) -> R<()> {
    let c = lk!(db);
    x(&c, "INSERT INTO cards(list_id,title,description,due,priority,pos,recur,fields) SELECT list_id,title||' (copy)',description,due,priority,pos+0.001,recur,fields FROM cards WHERE id=?", params![id])?;
    let n = c.last_insert_rowid();
    x(&c, "INSERT INTO checklist_items(card_id,text,done,pos) SELECT ?,text,0,pos FROM checklist_items WHERE card_id=?", params![n, id])?;
    x(&c, "INSERT INTO card_labels(card_id,label_id) SELECT ?,label_id FROM card_labels WHERE card_id=?", params![n, id])
}
#[tauri::command] fn archive_card(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "UPDATE cards SET archived=1,updated=CURRENT_TIMESTAMP WHERE id=?", params![id]) }
#[tauri::command] fn del_card(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM cards WHERE id=?", params![id]) }
#[tauri::command] fn card_detail(db: State<Db>, id: i64) -> R<Value> {
    let c = lk!(db);
    let bsel = "(SELECT l2.board_id FROM cards c2 JOIN lists l2 ON l2.id=c2.list_id WHERE c2.id=?1)";
    Ok(json!({
        "card": q(&c, "SELECT * FROM cards WHERE id=?", &[&id])?.into_iter().next(),
        "checklist": q(&c, "SELECT * FROM checklist_items WHERE card_id=? ORDER BY pos,id", &[&id])?,
        "attachments": q(&c, "SELECT id,name FROM attachments WHERE card_id=?", &[&id])?,
        "deps": q(&c, "SELECT x.id,x.title,x.done FROM card_deps d JOIN cards x ON x.id=d.depends_on WHERE d.card_id=?", &[&id])?,
        "labels": q(&c, "SELECT label_id FROM card_labels WHERE card_id=?", &[&id])?,
        "activity": q(&c, "SELECT text,at FROM activity WHERE card_id=? ORDER BY id DESC LIMIT 30", &[&id])?,
        "boardLabels": q(&c, &format!("SELECT * FROM labels WHERE board_id={bsel}"), &[&id])?,
        "others": q(&c, &format!("SELECT c.id,c.title FROM cards c JOIN lists l ON l.id=c.list_id WHERE l.board_id={bsel} AND c.id<>?1 AND c.archived=0 ORDER BY c.title"), &[&id])?
    }))
}
#[tauri::command] fn add_check(db: State<Db>, card: i64, text: String) -> R<()> {
    x(cn!(db), "INSERT INTO checklist_items(card_id,text,pos) VALUES(?,?,COALESCE((SELECT MAX(pos)+1 FROM checklist_items WHERE card_id=?),1))", params![card, clean(text, 300)?, card]) }
#[tauri::command] fn toggle_check(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "UPDATE checklist_items SET done=1-done WHERE id=?", params![id]) }
#[tauri::command] fn del_check(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM checklist_items WHERE id=?", params![id]) }
#[tauri::command] fn add_label(db: State<Db>, board: i64, name: String, color: String) -> R<()> {
    x(cn!(db), "INSERT INTO labels(board_id,name,color) VALUES(?,?,?)", params![board, clean(name.replace(['|', ';'], ""), 30)?, color]) }
#[tauri::command] fn del_label(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM labels WHERE id=?", params![id]) }
#[tauri::command] fn toggle_label(db: State<Db>, card: i64, label: i64) -> R<()> {
    let c = lk!(db);
    if c.execute("DELETE FROM card_labels WHERE card_id=? AND label_id=?", params![card, label]).map_err(e)? == 0 {
        x(&c, "INSERT INTO card_labels(card_id,label_id) VALUES(?,?)", params![card, label])?; }
    Ok(())
}
#[tauri::command] fn add_dep(db: State<Db>, card: i64, dep: i64) -> R<()> {
    if card == dep { return Err("A card can't depend on itself".into()); }
    x(cn!(db), "INSERT OR IGNORE INTO card_deps(card_id,depends_on) VALUES(?,?)", params![card, dep]) }
#[tauri::command] fn del_dep(db: State<Db>, card: i64, dep: i64) -> R<()> { x(cn!(db), "DELETE FROM card_deps WHERE card_id=? AND depends_on=?", params![card, dep]) }
#[tauri::command] fn add_comment(db: State<Db>, card: i64, text: String) -> R<()> { log(cn!(db), card, &format!("💬 {}", clean(text, 1000)?)); Ok(()) }
#[tauri::command] fn add_attachment(db: State<Db>, card: i64, name: String, data: String) -> R<()> {
    let bytes = STANDARD.decode(data).map_err(e)?;
    if bytes.len() > 25_000_000 { return Err("File too large (25 MB max)".into()); }
    let safe: String = name.chars().map(|ch| if ch.is_alphanumeric() || ch == '.' || ch == '-' || ch == '_' { ch } else { '_' }).collect();
    let dir = db.1.join("attachments"); fs::create_dir_all(&dir).map_err(e)?;
    let c = lk!(db);
    let stamp: i64 = c.query_row("SELECT CAST(strftime('%s','now') AS INTEGER)*1000+ABS(RANDOM())%1000", [], |r| r.get(0)).map_err(e)?;
    let p = dir.join(format!("{stamp}-{safe}"));
    fs::write(&p, bytes).map_err(e)?;
    x(&c, "INSERT INTO attachments(card_id,name,path) VALUES(?,?,?)", params![card, name, p.to_string_lossy().to_string()])
}
#[tauri::command] fn open_attachment(db: State<Db>, id: i64) -> R<()> {
    let p: String = lk!(db).query_row("SELECT path FROM attachments WHERE id=?", [id], |r| r.get(0)).map_err(e)?; open_it(Path::new(&p)) }
#[tauri::command] fn del_attachment(db: State<Db>, id: i64) -> R<()> {
    let c = lk!(db);
    if let Ok(p) = c.query_row("SELECT path FROM attachments WHERE id=?", [id], |r| r.get::<_, String>(0)) { let _ = fs::remove_file(p); }
    x(&c, "DELETE FROM attachments WHERE id=?", params![id])
}
#[tauri::command] fn save_template(db: State<Db>, card: i64, name: String) -> R<()> {
    let c = lk!(db);
    let cd = q(&c, "SELECT title,description,priority,recur,fields FROM cards WHERE id=?", &[&card])?.into_iter().next().ok_or("No such card")?;
    let ck: Vec<Value> = q(&c, "SELECT text FROM checklist_items WHERE card_id=? ORDER BY pos,id", &[&card])?.into_iter().map(|r| r["text"].clone()).collect();
    let mut d = cd; d["checklist"] = json!(ck);
    x(&c, "INSERT INTO templates(name,data) VALUES(?,?)", params![clean(name, 80)?, d.to_string()])
}
#[tauri::command] fn templates(db: State<Db>) -> R<Vec<Value>> { q(cn!(db), "SELECT id,name FROM templates ORDER BY name", &[]) }
#[tauri::command] fn del_template(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM templates WHERE id=?", params![id]) }
#[tauri::command] fn use_template(db: State<Db>, id: i64, list: i64) -> R<()> {
    let c = lk!(db);
    let data: String = c.query_row("SELECT data FROM templates WHERE id=?", [id], |r| r.get(0)).map_err(e)?;
    let t: Value = serde_json::from_str(&data).map_err(e)?;
    x(&c, "INSERT INTO cards(list_id,title,description,priority,pos,recur,fields) VALUES(?,?,?,?,COALESCE((SELECT MAX(pos)+1 FROM cards WHERE list_id=?),1),?,?)",
        params![list, t["title"].as_str().unwrap_or("Untitled"), t["description"].as_str().unwrap_or(""), t["priority"].as_i64().unwrap_or(0), list, t["recur"].as_str().unwrap_or(""), t["fields"].as_str().unwrap_or("")])?;
    let n = c.last_insert_rowid();
    if let Some(a) = t["checklist"].as_array() { for (i, s) in a.iter().enumerate() {
        x(&c, "INSERT INTO checklist_items(card_id,text,pos) VALUES(?,?,?)", params![n, s.as_str().unwrap_or(""), i as f64])?; } }
    Ok(())
}
#[tauri::command] fn calendar(db: State<Db>, from: String, to: String) -> R<Vec<Value>> {
    q(cn!(db), "SELECT c.id,c.title,c.due,c.done,c.priority,b.name AS board FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=0 AND c.due BETWEEN ? AND ? ORDER BY c.due", &[&from, &to]) }
#[tauri::command] fn search(db: State<Db>, text: String) -> R<Vec<Value>> {
    let like = format!("%{}%", text.replace('%', ""));
    q(cn!(db), "SELECT c.id,c.title,c.due,b.id AS board_id,b.name AS board FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=0 AND (c.title LIKE ?1 OR c.description LIKE ?1 OR c.fields LIKE ?1) LIMIT 50", &[&like]) }
#[tauri::command] fn dashboard(db: State<Db>, today: String) -> R<Value> {
    let c = lk!(db);
    let ids: Vec<i64> = gs(&c, "recent").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    let mut rb = if ids.is_empty() { vec![] } else { q(&c, &format!("SELECT id AS board_id,name AS board FROM boards WHERE id IN ({})", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")), &[])? };
    rb.sort_by_key(|r| ids.iter().position(|i| Some(*i) == r["board_id"].as_i64()));
    let base = "SELECT c.id,c.title,c.due,b.id AS board_id,b.name AS board FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=0 AND c.done=0 AND c.due IS NOT NULL";
    Ok(json!({
        "overdue": q(&c, &format!("{base} AND c.due<? ORDER BY c.due"), &[&today])?,
        "today": q(&c, &format!("{base} AND c.due=? ORDER BY c.priority DESC"), &[&today])?,
        "upcoming": q(&c, &format!("{base} AND c.due>? ORDER BY c.due LIMIT 15"), &[&today])?,
        "recent": q(&c, "SELECT c.id,c.title,b.id AS board_id,b.name AS board FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=0 ORDER BY c.updated DESC LIMIT 8", &[])?,
        "inbox": q(&c, "SELECT * FROM inbox ORDER BY id DESC", &[])?,
        "favs": q(&c, "SELECT id AS board_id,name AS board FROM boards WHERE fav=1 ORDER BY name", &[])?,
        "recent_boards": rb,
        "milestones": q(&c, "SELECT m.*,b.name AS board,b.id AS board_id FROM milestones m JOIN boards b ON b.id=m.board_id WHERE m.done=0 ORDER BY m.due LIMIT 10", &[])?,
        "progress": q(&c, "SELECT b.id AS board_id,b.name AS board,COUNT(c.id) AS total,COALESCE(SUM(c.done),0) AS done FROM boards b LEFT JOIN lists l ON l.board_id=b.id LEFT JOIN cards c ON c.list_id=l.id AND c.archived=0 GROUP BY b.id ORDER BY total DESC LIMIT 12", &[])?
    }))
}
#[tauri::command] fn add_inbox(db: State<Db>, text: String) -> R<()> { x(cn!(db), "INSERT INTO inbox(text) VALUES(?)", params![clean(text, 500)?]) }
#[tauri::command] fn del_inbox(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM inbox WHERE id=?", params![id]) }
#[tauri::command] fn settings(db: State<Db>) -> R<Vec<Value>> { q(cn!(db), "SELECT * FROM settings", &[]) }
#[tauri::command] fn set_setting(db: State<Db>, key: String, value: String) -> R<()> { x(cn!(db), "INSERT OR REPLACE INTO settings(key,value) VALUES(?,?)", params![key, value]) }
#[tauri::command] fn export_file(db: State<Db>) -> R<String> {
    let c = lk!(db);
    let ts: String = c.query_row("SELECT strftime('%Y%m%d-%H%M%S','now')", [], |r| r.get(0)).map_err(e)?;
    let dir = db.1.join("exports"); fs::create_dir_all(&dir).map_err(e)?;
    let f = dir.join(format!("project-manager-{ts}.json"));
    fs::write(&f, serde_json::to_string_pretty(&dump(&c)?).map_err(e)?).map_err(e)?;
    let _ = open_it(&dir); Ok(f.to_string_lossy().to_string())
}
#[tauri::command] fn import_json(db: State<Db>, text: String) -> R<()> {
    let v: Value = serde_json::from_str(&text).map_err(|x| format!("Invalid JSON: {x}"))?;
    let c = lk!(db); make_backup(&c, &db.1, "-pre-import")?; load(&c, &v)
}
#[tauri::command] fn backup(db: State<Db>) -> R<String> { Ok(make_backup(cn!(db), &db.1, "")?.to_string_lossy().to_string()) }
#[tauri::command] fn backups(db: State<Db>) -> Vec<String> { backup_names(&db.1) }
#[tauri::command] fn restore(db: State<Db>, name: String) -> R<()> {
    if !backup_names(&db.1).contains(&name) { return Err("Unknown backup".into()); }
    let c = lk!(db); make_backup(&c, &db.1, "-pre-restore")?;
    let other = Connection::open(db.1.join("backups").join(&name)).map_err(e)?;
    load(&c, &dump(&other)?)
}
#[tauri::command] fn open_data_folder(db: State<Db>) -> R<()> { open_it(&db.1) }
#[tauri::command] fn seed(db: State<Db>) -> R<()> {
    let c = lk!(db);
    let s: &[(&str, &str, &str, &[(&str, &str)])] = &[
        ("Software Engineering", "💻", "#6c8cff", &[("Projects", ""), ("Debugging & Experiments", "Inbox,Investigating,Fixed")]),
        ("Cybersecurity", "🛡️", "#ef4444", &[("Learning Resources", "To read,Reading,Notes done"), ("Research & Experiments", "Ideas,Lab,Write-up,Done")]),
        ("Web Development", "🌐", "#22d3ee", &[("Websites", "Backlog,Building,Deployed,Maintenance")]),
        ("Content Creation", "🎬", "#f59e0b", &[("YouTube Pipeline", "Ideas,Scripting,Filming,Editing,Scheduled,Published"), ("Social Media", "Ideas,Drafted,Scheduled,Posted")]),
        ("Game Development", "🎮", "#a855f7", &[("Game Dev", "Concepts,Todo,In progress,Testing,Released")]),
        ("Creative Projects", "🎨", "#ec4899", &[("VTuber & Modelling", "Ideas,Design,Modelling,Rigging,Done"), ("Branding & Assets", "")]),
        ("Education", "📚", "#10b981", &[("Courses & Certs", "Planned,Studying,Completed"), ("Study Plan", "")]),
        ("Personal", "🏠", "#94a3b8", &[("Tasks", "To do,Today,Waiting,Done"), ("Deadlines & Documents", "")]),
    ];
    for (n, ic, col, bs) in s {
        x(&c, "INSERT INTO workspaces(name,color,icon) VALUES(?,?,?)", params![n, col, ic])?;
        let ws = c.last_insert_rowid();
        for (bn, ls) in bs.iter() { new_board(&c, ws, bn, "#1f2937", ls)?; }
    }
    Ok(())
}

fn gs(c: &Connection, k: &str) -> Option<String> { c.query_row("SELECT value FROM settings WHERE key=?", [k], |r| r.get(0)).ok() }
fn ss(c: &Connection, k: &str, v: &str) -> R<()> { x(c, "INSERT OR REPLACE INTO settings(key,value) VALUES(?,?)", params![k, v]) }
fn fnv(s: &str) -> String { let mut h: u64 = 0xcbf29ce484222325; for b in s.bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); } format!("{h:016x}") }
fn local_key(k: &str) -> bool { k.starts_with("sync_") || k.starts_with("notify") || k == "last_notified" || k == "recent" }
fn base_name(p: &str) -> &str { p.rsplit(|ch| ch == '/' || ch == '\\').next().unwrap_or(p) }
fn sync_dump(c: &Connection) -> R<Value> {
    let mut d = dump(c)?;
    if let Some(a) = d["settings"].as_array_mut() { a.retain(|r| !local_key(r["key"].as_str().unwrap_or(""))); }
    if let Some(a) = d["attachments"].as_array_mut() { for r in a.iter_mut() { let b = base_name(r["path"].as_str().unwrap_or("")).to_string(); r["path"] = json!(b); } }
    Ok(d)
}
fn sync_core(c: &Connection, app: &Path, mode: &str) -> R<String> {
    let dir = PathBuf::from(gs(c, "sync_dir").ok_or("Set a sync folder first")?);
    fs::create_dir_all(dir.join("attachments")).map_err(e)?;
    fs::create_dir_all(app.join("attachments")).map_err(e)?;
    let file = dir.join("command-centre-sync.json");
    let local = sync_dump(c)?; let lh = fnv(&local.to_string());
    let last = gs(c, "sync_hash").unwrap_or_default();
    let remote: Option<(Value, String)> = if file.exists() {
        let v: Value = serde_json::from_str(&fs::read_to_string(&file).map_err(e)?).map_err(|m| format!("Sync file unreadable: {m}"))?;
        let d = v.get("data").cloned().ok_or("Sync file has no data")?; let h = fnv(&d.to_string()); Some((d, h))
    } else { None };
    let empty_local = local["workspaces"].as_array().map(|a| a.is_empty()).unwrap_or(true);
    let action = match (mode, &remote) {
        ("push", _) => "push",
        ("pull", Some(_)) => "pull",
        ("pull", None) => return Err("Nothing to pull yet".into()),
        (_, None) => "push",
        (_, Some((_, rh))) => if *rh == lh { "same" } else if last.is_empty() && empty_local { "pull" } else if *rh == last { "push" } else if lh == last { "pull" } else { "conflict" },
    };
    match action {
        "same" => { ss(c, "sync_hash", &lh)?; Ok("same".into()) }
        "push" => {
            for r in q(c, "SELECT path FROM attachments", &[])? { if let Some(p) = r["path"].as_str() { let _ = fs::copy(p, dir.join("attachments").join(base_name(p))); } }
            let ts: String = c.query_row("SELECT strftime('%Y-%m-%d %H:%M:%S','now')", [], |r| r.get(0)).map_err(e)?;
            let tmp = dir.join("command-centre-sync.json.tmp");
            fs::write(&tmp, json!({"app": "command-centre", "saved_at": ts, "data": local}).to_string()).map_err(e)?;
            fs::rename(&tmp, &file).map_err(e)?;
            ss(c, "sync_hash", &lh)?; Ok("pushed".into())
        }
        "pull" => {
            let (d, rh) = remote.ok_or("Nothing to pull")?;
            make_backup(c, app, "-pre-sync")?;
            let keep = q(c, "SELECT key,value FROM settings WHERE key LIKE 'sync\\_%' ESCAPE '\\' OR key LIKE 'notify%' OR key='last_notified'", &[])?;
            load(c, &d)?;
            for r in q(c, "SELECT id,path FROM attachments", &[])? {
                let n = base_name(r["path"].as_str().unwrap_or("")).to_string();
                let np = app.join("attachments").join(&n);
                let _ = fs::copy(dir.join("attachments").join(&n), &np);
                x(c, "UPDATE attachments SET path=? WHERE id=?", params![np.to_string_lossy().to_string(), r["id"].as_i64().unwrap_or(0)])?;
            }
            for r in keep { ss(c, r["key"].as_str().unwrap_or(""), r["value"].as_str().unwrap_or(""))?; }
            ss(c, "sync_hash", &rh)?; Ok("pulled".into())
        }
        _ => Ok("conflict".into()),
    }
}
#[tauri::command] fn sync_set(db: State<Db>, dir: String) -> R<()> {
    let d = dir.trim().to_string(); let c = lk!(db);
    if d.is_empty() { x(&c, "DELETE FROM settings WHERE key IN ('sync_dir','sync_hash')", &[]) } else { fs::create_dir_all(&d).map_err(e)?; ss(&c, "sync_dir", &d) }
}
#[tauri::command] fn sync_now(db: State<Db>, mode: String) -> R<String> { let c = lk!(db); sync_core(&c, &db.1, &mode) }

fn notify_check(app: &tauri::AppHandle, force: bool) -> R<()> {
    let st = app.state::<Db>(); let c = st.0.lock().map_err(e)?;
    let (hour_now, today): (i64, String) = c.query_row("SELECT CAST(strftime('%H','now','localtime') AS INTEGER),date('now','localtime')", [], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?;
    if !force {
        if gs(&c, "notify").as_deref() == Some("0") { return Ok(()); }
        let hour: i64 = gs(&c, "notify_hour").and_then(|h| h.parse().ok()).unwrap_or(9);
        if hour_now < hour || gs(&c, "last_notified").as_deref() == Some(today.as_str()) { return Ok(()); }
    }
    let n = |sql: &str| -> i64 { c.query_row(sql, params![today], |r| r.get(0)).unwrap_or(0) };
    let over = n("SELECT COUNT(*) FROM cards WHERE archived=0 AND done=0 AND due<?");
    let due = n("SELECT COUNT(*) FROM cards WHERE archived=0 AND done=0 AND due=?");
    let ms = n("SELECT COUNT(*) FROM milestones WHERE done=0 AND due<=?");
    if over + due + ms == 0 && !force { return ss(&c, "last_notified", &today); }
    let titles: Vec<String> = q(&c, "SELECT title FROM cards WHERE archived=0 AND done=0 AND due<=? ORDER BY due LIMIT 3", &[&today])?.into_iter().map(|r| r["title"].as_str().unwrap_or("").to_string()).collect();
    let body = format!("{over} overdue · {due} due today{}\n{}", if ms > 0 { format!(" · {ms} milestone(s)") } else { String::new() }, titles.join(", "));
    app.notification().builder().title("Project Manager").body(body).show().map_err(e)?;
    if !force { ss(&c, "last_notified", &today)?; }
    Ok(())
}
#[tauri::command] fn test_notify(app: tauri::AppHandle) -> R<()> { notify_check(&app, true) }

#[tauri::command] fn archived(db: State<Db>) -> R<Vec<Value>> {
    q(cn!(db), "SELECT c.id,c.title,c.due,b.id AS board_id,b.name AS board,l.name AS list FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=1 ORDER BY c.updated DESC LIMIT 300", &[]) }
#[tauri::command] fn unarchive(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "UPDATE cards SET archived=0,updated=CURRENT_TIMESTAMP WHERE id=?", params![id]) }
#[tauri::command] fn milestones(db: State<Db>, board: i64) -> R<Vec<Value>> { q(cn!(db), "SELECT * FROM milestones WHERE board_id=? ORDER BY due", &[&board]) }
#[tauri::command] fn add_milestone(db: State<Db>, board: i64, name: String, due: String) -> R<()> {
    if due.len() != 10 { return Err("Pick a date".into()); }
    x(cn!(db), "INSERT INTO milestones(board_id,name,due) VALUES(?,?,?)", params![board, clean(name, 100)?, due]) }
#[tauri::command] fn toggle_milestone(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "UPDATE milestones SET done=1-done WHERE id=?", params![id]) }
#[tauri::command] fn del_milestone(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM milestones WHERE id=?", params![id]) }
#[tauri::command] fn calendar_ms(db: State<Db>, from: String, to: String) -> R<Vec<Value>> {
    q(cn!(db), "SELECT m.*,b.name AS board FROM milestones m JOIN boards b ON b.id=m.board_id WHERE m.due BETWEEN ? AND ? ORDER BY m.due", &[&from, &to]) }
#[tauri::command] fn timeline(db: State<Db>, board: i64) -> R<Value> {
    let c = lk!(db);
    Ok(json!({
        "cards": q(&c, "SELECT c.id,c.title,c.done,COALESCE(NULLIF(c.start,''),date(c.created)) AS start,c.due FROM cards c JOIN lists l ON l.id=c.list_id WHERE l.board_id=? AND c.archived=0 AND c.due IS NOT NULL ORDER BY start,c.due", &[&board])?,
        "milestones": q(&c, "SELECT * FROM milestones WHERE board_id=? ORDER BY due", &[&board])?
    }))
}
#[tauri::command] fn filters(db: State<Db>) -> R<Vec<Value>> { q(cn!(db), "SELECT * FROM filters ORDER BY name", &[]) }
#[tauri::command] fn save_filter(db: State<Db>, name: String, data: String) -> R<()> { x(cn!(db), "INSERT INTO filters(name,data) VALUES(?,?)", params![clean(name, 60)?, data]) }
#[tauri::command] fn del_filter(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "DELETE FROM filters WHERE id=?", params![id]) }

#[tauri::command] fn set_list_color(db: State<Db>, id: i64, color: String) -> R<()> { x(cn!(db), "UPDATE lists SET color=? WHERE id=?", params![color, id]) }
#[tauri::command] fn set_list_pos(db: State<Db>, id: i64, pos: f64) -> R<()> { x(cn!(db), "UPDATE lists SET pos=? WHERE id=?", params![pos, id]) }
#[tauri::command] fn archive_cards(db: State<Db>, list: i64, done_only: bool) -> R<()> {
    x(cn!(db), "UPDATE cards SET archived=1,updated=CURRENT_TIMESTAMP WHERE list_id=? AND archived=0 AND (?=0 OR done=1)", params![list, done_only as i64]) }
#[tauri::command] fn toggle_fav(db: State<Db>, id: i64) -> R<()> { x(cn!(db), "UPDATE boards SET fav=1-fav WHERE id=?", params![id]) }
#[tauri::command] fn timer_toggle(db: State<Db>, id: i64) -> R<()> {
    x(cn!(db), "UPDATE cards SET spent=spent+CASE WHEN timer>0 THEN CAST(strftime('%s','now') AS INTEGER)-timer ELSE 0 END,timer=CASE WHEN timer>0 THEN 0 ELSE CAST(strftime('%s','now') AS INTEGER) END WHERE id=?", params![id]) }
#[tauri::command] fn touch_board(db: State<Db>, id: i64) -> R<()> {
    let c = lk!(db); let me = id.to_string();
    let mut v: Vec<String> = gs(&c, "recent").unwrap_or_default().split(',').filter(|s| !s.is_empty() && *s != me).map(String::from).collect();
    v.insert(0, me); v.truncate(6); ss(&c, "recent", &v.join(","))
}
#[tauri::command] fn notify_text(app: tauri::AppHandle, title: String, body: String) -> R<()> { app.notification().builder().title(title).body(body).show().map_err(e) }
#[tauri::command] fn export_text(db: State<Db>, name: String, text: String) -> R<String> {
    let safe: String = name.chars().map(|ch| if ch.is_alphanumeric() || ch == '-' || ch == '_' || ch == '.' { ch } else { '_' }).collect();
    let dir = db.1.join("exports"); fs::create_dir_all(&dir).map_err(e)?;
    let f = dir.join(safe); fs::write(&f, text).map_err(e)?; let _ = open_it(&dir); Ok(f.to_string_lossy().to_string())
}

#[tauri::command] fn set_priority(db: State<Db>, id: i64, priority: i64) -> R<()> { x(cn!(db), "UPDATE cards SET priority=?,updated=CURRENT_TIMESTAMP WHERE id=?", params![priority.clamp(0, 3), id]) }
#[tauri::command] fn set_due(db: State<Db>, id: i64, due: Option<String>) -> R<()> { x(cn!(db), "UPDATE cards SET due=?,updated=CURRENT_TIMESTAMP WHERE id=?", params![due.filter(|d| !d.is_empty()), id]) }
#[tauri::command] fn ws_lists(db: State<Db>, ws: i64) -> R<Vec<Value>> {
    q(cn!(db), "SELECT l.id,l.name AS list,b.name AS board FROM lists l JOIN boards b ON b.id=l.board_id WHERE b.workspace_id=? ORDER BY b.id,l.pos LIMIT 80", &[&ws]) }

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            fs::create_dir_all(&dir)?;
            let c = Connection::open(dir.join("data.db"))?;
            c.execute_batch(include_str!("schema.sql"))?;
            for m in ["ALTER TABLE cards ADD COLUMN done INTEGER NOT NULL DEFAULT 0", "ALTER TABLE cards ADD COLUMN recur TEXT NOT NULL DEFAULT ''", "ALTER TABLE cards ADD COLUMN fields TEXT NOT NULL DEFAULT ''", "ALTER TABLE cards ADD COLUMN start TEXT", "ALTER TABLE lists ADD COLUMN color TEXT NOT NULL DEFAULT ''", "ALTER TABLE boards ADD COLUMN fav INTEGER NOT NULL DEFAULT 0", "ALTER TABLE cards ADD COLUMN est INTEGER NOT NULL DEFAULT 0", "ALTER TABLE cards ADD COLUMN spent INTEGER NOT NULL DEFAULT 0", "ALTER TABLE cards ADD COLUMN timer INTEGER NOT NULL DEFAULT 0"] { let _ = c.execute(m, []); }
            let _ = make_backup(&c, &dir, "-auto");
            for old in backup_names(&dir).into_iter().skip(20) { let _ = fs::remove_file(dir.join("backups").join(old)); }
            app.manage(Db(Mutex::new(c), dir));
            let h = app.handle().clone();
            std::thread::spawn(move || loop { let _ = notify_check(&h, false); std::thread::sleep(std::time::Duration::from_secs(300)); });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![workspaces, add_workspace, edit_workspace, del_workspace, boards, add_board, edit_board, dup_board, del_board, board,
            add_list, rename_list, move_list, del_list, add_card, move_card, update_card, toggle_done, dup_card, archive_card, del_card, card_detail,
            add_check, toggle_check, del_check, add_label, del_label, toggle_label, add_dep, del_dep, add_comment, add_attachment, open_attachment, del_attachment,
            save_template, templates, del_template, use_template, calendar, search, dashboard, add_inbox, del_inbox, settings, set_setting,
            export_file, import_json, backup, backups, restore, open_data_folder, seed, archived, unarchive, milestones, add_milestone, toggle_milestone, del_milestone, calendar_ms, timeline, filters, save_filter, del_filter, sync_set, sync_now, test_notify, set_list_color, set_list_pos, archive_cards, toggle_fav, timer_toggle, touch_board, notify_text, export_text, set_priority, set_due, ws_lists])
        .run(tauri::generate_context!())
        .expect("error while running app");
}
