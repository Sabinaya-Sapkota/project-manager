/* Browser backend for the web demo: implements the same commands as the desktop (Rust) backend, on top of SQLite compiled to WebAssembly. */
(function (root) {
'use strict';
const TABLES = ["workspaces","boards","lists","cards","labels","card_labels","checklist_items","attachments","card_deps","activity","inbox","templates","settings","milestones","filters"];
const CARDS_SQL = `SELECT c.*,
 (SELECT COUNT(*) FROM checklist_items WHERE card_id=c.id) AS ck_total,
 (SELECT COUNT(*) FROM checklist_items WHERE card_id=c.id AND done=1) AS ck_done,
 (SELECT group_concat(lb.color||'|'||lb.name||'|'||lb.id,';') FROM card_labels cl JOIN labels lb ON lb.id=cl.label_id WHERE cl.card_id=c.id) AS labels,
 (SELECT COUNT(*) FROM card_deps d JOIN cards x ON x.id=d.depends_on WHERE d.card_id=c.id AND x.done=0) AS blocked,
 (SELECT COUNT(*) FROM attachments WHERE card_id=c.id) AS att
 FROM cards c JOIN lists l ON l.id=c.list_id WHERE l.board_id=? AND c.archived=0 ORDER BY c.pos`;
const SEED = [
 ['Software Engineering','💻','#6c8cff',[['Projects',''],['Debugging & Experiments','Inbox,Investigating,Fixed']]],
 ['Cybersecurity','🛡️','#ef4444',[['Learning Resources','To read,Reading,Notes done'],['Research & Experiments','Ideas,Lab,Write-up,Done']]],
 ['Web Development','🌐','#22d3ee',[['Websites','Backlog,Building,Deployed,Maintenance']]],
 ['Content Creation','🎬','#f59e0b',[['YouTube Pipeline','Ideas,Scripting,Filming,Editing,Scheduled,Published'],['Social Media','Ideas,Drafted,Scheduled,Posted']]],
 ['Game Development','🎮','#a855f7',[['Game Dev','Concepts,Todo,In progress,Testing,Released']]],
 ['Creative Projects','🎨','#ec4899',[['VTuber & Modelling','Ideas,Design,Modelling,Rigging,Done'],['Branding & Assets','']]],
 ['Education','📚','#10b981',[['Courses & Certs','Planned,Studying,Completed'],['Study Plan','']]],
 ['Personal','🏠','#94a3b8',[['Tasks','To do,Today,Waiting,Done'],['Deadlines & Documents','']]]
];
const NA = 'Not available in the web demo (desktop app only)';
const b64 = u => { let s = ''; for (let i = 0; i < u.length; i += 0x8000) s += String.fromCharCode.apply(null, u.subarray(i, i + 0x8000)); return btoa(s); };
const unb64 = s => { const b = atob(s), u = new Uint8Array(b.length); for (let i = 0; i < b.length; i++) u[i] = b.charCodeAt(i); return u; };
const stamp = () => new Date().toISOString().replace(/[-:]/g, '').slice(0, 15).replace('T', '-');
const clean = (s, max) => { s = String(s == null ? '' : s).trim(); if (!s || [...s].length > max) throw `Text must be 1-${max} characters`; return s; };
const icon4 = s => [...String(s || '')].slice(0, 4).join('');

function make(SQL, store, schema, env) {
  env = env || {};
  let db;
  const open = bytes => { db = bytes ? new SQL.Database(bytes) : new SQL.Database(); db.exec(schema); };
  const Q = (sql, p) => { const st = db.prepare(sql); if (p && p.length) st.bind(p); const r = []; while (st.step()) r.push(st.getAsObject()); st.free(); return r; };
  const X = (sql, p) => { db.run(sql, p || []); };
  const lastId = () => Q('SELECT last_insert_rowid() AS i')[0].i;
  const log = (card, t) => X('INSERT INTO activity(card_id,text) VALUES(?,?)', [card, t]);
  const gs = k => { const r = Q('SELECT value FROM settings WHERE key=?', [k])[0]; return r ? r.value : undefined; };
  const ss = (k, v) => X('INSERT OR REPLACE INTO settings(key,value) VALUES(?,?)', [k, String(v)]);
  const save = () => { store.set('db', b64(db.export())); db.exec('PRAGMA foreign_keys=ON'); };
  const snap = () => { const d = b64(db.export()); db.exec('PRAGMA foreign_keys=ON'); return d; };
  const dump = () => { const o = {}; TABLES.forEach(t => o[t] = Q('SELECT * FROM ' + t)); return o; };
  const load = v => {
    if (!v || typeof v !== 'object' || Array.isArray(v)) throw 'Not a Project Manager export';
    db.exec('PRAGMA foreign_keys=OFF;BEGIN');
    try {
      [...TABLES].reverse().forEach(t => db.run('DELETE FROM ' + t));
      for (const t of TABLES) for (const r of (Array.isArray(v[t]) ? v[t] : [])) {
        const k = Object.keys(r);
        if (!k.length || k.some(x => !/^[A-Za-z0-9_]+$/.test(x))) throw 'bad column name';
        db.run(`INSERT INTO ${t}(${k.join(',')}) VALUES(${k.map(() => '?').join(',')})`, k.map(x => r[x] == null ? null : r[x]));
      }
      db.exec('COMMIT');
    } catch (e) { try { db.exec('ROLLBACK'); } catch (_) {} db.exec('PRAGMA foreign_keys=ON'); throw e; }
    db.exec('PRAGMA foreign_keys=ON');
  };
  const newBoard = (ws, name, bg, lists) => {
    X('INSERT INTO boards(workspace_id,name,bg) VALUES(?,?,?)', [ws, name, bg || '#1f2937']);
    const id = lastId();
    (lists ? lists.split(',') : ['Backlog', 'In progress', 'Done']).forEach((n, i) => X('INSERT INTO lists(board_id,name,pos) VALUES(?,?,?)', [id, n.trim(), i + 1]));
    [['Bug','#f87171'],['Idea','#fbbf24'],['Urgent','#fb923c'],['Research','#a78bfa'],['Content','#34d399'],['Design','#f472b6']].forEach(([n, c]) => X('INSERT INTO labels(board_id,name,color) VALUES(?,?,?)', [id, n, c]));
    return id;
  };
  const maxPos = (t, col, id) => (Q(`SELECT COALESCE(MAX(pos)+1,1) AS p FROM ${t} WHERE ${col}=?`, [id])[0]).p;
  const dl = (name, text, mime) => { if (env.download) env.download(name, text, mime); };
  const bsel = '(SELECT l2.board_id FROM cards c2 JOIN lists l2 ON l2.id=c2.list_id WHERE c2.id=?1)';

  const C = {
    workspaces: () => Q('SELECT * FROM workspaces ORDER BY id'),
    add_workspace: a => X('INSERT INTO workspaces(name,color,icon) VALUES(?,?,?)', [clean(a.name, 60), a.color, icon4(a.icon)]),
    edit_workspace: a => X('UPDATE workspaces SET name=?,color=?,icon=? WHERE id=?', [clean(a.name, 60), a.color, icon4(a.icon), a.id]),
    del_workspace: a => X('DELETE FROM workspaces WHERE id=?', [a.id]),
    boards: a => Q('SELECT * FROM boards WHERE workspace_id=? ORDER BY id', [a.ws]),
    add_board: a => newBoard(a.ws, clean(a.name, 80), a.bg, ''),
    edit_board: a => X('UPDATE boards SET name=?,bg=? WHERE id=?', [clean(a.name, 80), a.bg, a.id]),
    dup_board: a => {
      X("INSERT INTO boards(workspace_id,name,bg) SELECT workspace_id,name||' (copy)',bg FROM boards WHERE id=?", [a.id]);
      const n = lastId();
      X('INSERT INTO lists(board_id,name,pos) SELECT ?,name,pos FROM lists WHERE board_id=?', [n, a.id]);
      X('INSERT INTO labels(board_id,name,color) SELECT ?,name,color FROM labels WHERE board_id=?', [n, a.id]);
      return n;
    },
    del_board: a => X('DELETE FROM boards WHERE id=?', [a.id]),
    board: a => ({ board: Q('SELECT * FROM boards WHERE id=?', [a.id])[0] || null, lists: Q('SELECT * FROM lists WHERE board_id=? ORDER BY pos', [a.id]), cards: Q(CARDS_SQL, [a.id]), labels: Q('SELECT * FROM labels WHERE board_id=? ORDER BY id', [a.id]) }),
    add_list: a => X('INSERT INTO lists(board_id,name,pos) VALUES(?,?,?)', [a.board, clean(a.name, 80), maxPos('lists', 'board_id', a.board)]),
    rename_list: a => X('UPDATE lists SET name=? WHERE id=?', [clean(a.name, 80), a.id]),
    move_list: a => {
      const l = Q('SELECT board_id,pos FROM lists WHERE id=?', [a.id])[0]; if (!l) return;
      const n = Q(a.dir < 0 ? 'SELECT id,pos FROM lists WHERE board_id=? AND pos<? ORDER BY pos DESC LIMIT 1' : 'SELECT id,pos FROM lists WHERE board_id=? AND pos>? ORDER BY pos ASC LIMIT 1', [l.board_id, l.pos])[0];
      if (n) { X('UPDATE lists SET pos=? WHERE id=?', [n.pos, a.id]); X('UPDATE lists SET pos=? WHERE id=?', [l.pos, n.id]); }
    },
    del_list: a => X('DELETE FROM lists WHERE id=?', [a.id]),
    add_card: a => { const t = clean(a.title, 300); X('INSERT INTO cards(list_id,title,pos) VALUES(?,?,?)', [a.list, t, maxPos('cards', 'list_id', a.list)]); log(lastId(), 'Created'); },
    move_card: a => { X('UPDATE cards SET list_id=?,pos=?,updated=CURRENT_TIMESTAMP WHERE id=?', [a.list, a.pos, a.id]); log(a.id, 'Moved'); },
    update_card: a => {
      if (!['', 'daily', 'weekly', 'monthly'].includes(a.recur)) throw 'Bad recurrence';
      X('UPDATE cards SET title=?,description=?,due=?,start=?,priority=?,recur=?,fields=?,est=?,updated=CURRENT_TIMESTAMP WHERE id=?',
        [clean(a.title, 300), a.description || '', a.due || null, a.start || null, Math.min(3, Math.max(0, a.priority | 0)), a.recur, a.fields || '', Math.max(0, a.est | 0), a.id]);
    },
    toggle_done: a => {
      const c = Q('SELECT recur,done FROM cards WHERE id=?', [a.id])[0]; if (!c) throw 'No such card';
      const bl = Q('SELECT COUNT(*) AS n FROM card_deps d JOIN cards x ON x.id=d.depends_on WHERE d.card_id=? AND x.done=0', [a.id])[0].n;
      if (bl > 0 && c.done == 0) throw 'Blocked: finish its dependencies first';
      if (c.done == 0 && c.recur) {
        const m = { weekly: '+7 days', monthly: '+1 month' }[c.recur] || '+1 day';
        X("UPDATE cards SET due=date(max(COALESCE(due,date('now')),date('now')),?),updated=CURRENT_TIMESTAMP WHERE id=?", [m, a.id]);
        X('UPDATE checklist_items SET done=0 WHERE card_id=?', [a.id]); log(a.id, 'Completed - rescheduled (recurring)');
      } else { X('UPDATE cards SET done=1-done,updated=CURRENT_TIMESTAMP WHERE id=?', [a.id]); log(a.id, c.done == 0 ? 'Marked done' : 'Reopened'); }
    },
    dup_card: a => {
      X("INSERT INTO cards(list_id,title,description,due,priority,pos,recur,fields) SELECT list_id,title||' (copy)',description,due,priority,pos+0.001,recur,fields FROM cards WHERE id=?", [a.id]);
      const n = lastId();
      X('INSERT INTO checklist_items(card_id,text,done,pos) SELECT ?,text,0,pos FROM checklist_items WHERE card_id=?', [n, a.id]);
      X('INSERT INTO card_labels(card_id,label_id) SELECT ?,label_id FROM card_labels WHERE card_id=?', [n, a.id]);
    },
    archive_card: a => X('UPDATE cards SET archived=1,updated=CURRENT_TIMESTAMP WHERE id=?', [a.id]),
    del_card: a => X('DELETE FROM cards WHERE id=?', [a.id]),
    card_detail: a => ({
      card: Q('SELECT * FROM cards WHERE id=?', [a.id])[0] || null,
      checklist: Q('SELECT * FROM checklist_items WHERE card_id=? ORDER BY pos,id', [a.id]),
      attachments: Q('SELECT id,name FROM attachments WHERE card_id=?', [a.id]),
      deps: Q('SELECT x.id,x.title,x.done FROM card_deps d JOIN cards x ON x.id=d.depends_on WHERE d.card_id=?', [a.id]),
      labels: Q('SELECT label_id FROM card_labels WHERE card_id=?', [a.id]),
      activity: Q('SELECT text,at FROM activity WHERE card_id=? ORDER BY id DESC LIMIT 30', [a.id]),
      boardLabels: Q(`SELECT * FROM labels WHERE board_id=${bsel}`, [a.id]),
      others: Q(`SELECT c.id,c.title FROM cards c JOIN lists l ON l.id=c.list_id WHERE l.board_id=${bsel} AND c.id<>?1 AND c.archived=0 ORDER BY c.title`, [a.id])
    }),
    add_check: a => X('INSERT INTO checklist_items(card_id,text,pos) VALUES(?,?,?)', [a.card, clean(a.text, 300), maxPos('checklist_items', 'card_id', a.card)]),
    toggle_check: a => X('UPDATE checklist_items SET done=1-done WHERE id=?', [a.id]),
    del_check: a => X('DELETE FROM checklist_items WHERE id=?', [a.id]),
    add_label: a => X('INSERT INTO labels(board_id,name,color) VALUES(?,?,?)', [a.board, clean(String(a.name || '').replace(/[|;]/g, ''), 30), a.color]),
    del_label: a => X('DELETE FROM labels WHERE id=?', [a.id]),
    toggle_label: a => {
      if (Q('SELECT 1 AS x FROM card_labels WHERE card_id=? AND label_id=?', [a.card, a.label]).length) X('DELETE FROM card_labels WHERE card_id=? AND label_id=?', [a.card, a.label]);
      else X('INSERT INTO card_labels(card_id,label_id) VALUES(?,?)', [a.card, a.label]);
    },
    add_dep: a => { if (a.card === a.dep) throw "A card can't depend on itself"; X('INSERT OR IGNORE INTO card_deps(card_id,depends_on) VALUES(?,?)', [a.card, a.dep]); },
    del_dep: a => X('DELETE FROM card_deps WHERE card_id=? AND depends_on=?', [a.card, a.dep]),
    add_comment: a => log(a.card, '💬 ' + clean(a.text, 1000)),
    add_attachment: a => {
      if (String(a.data).length > 1400000) throw 'Web demo limit: files up to 1 MB';
      X('INSERT INTO attachments(card_id,name,path) VALUES(?,?,?)', [a.card, a.name, 'browser']); store.set('att_' + lastId(), a.data);
    },
    open_attachment: a => { const r = Q('SELECT name FROM attachments WHERE id=?', [a.id])[0], d = store.get('att_' + a.id); if (!r || !d) throw 'Attachment not found'; if (env.openAtt) env.openAtt(r.name, d); },
    del_attachment: a => { store.del('att_' + a.id); X('DELETE FROM attachments WHERE id=?', [a.id]); },
    save_template: a => {
      const c = Q('SELECT title,description,priority,recur,fields FROM cards WHERE id=?', [a.card])[0]; if (!c) throw 'No such card';
      c.checklist = Q('SELECT text FROM checklist_items WHERE card_id=? ORDER BY pos,id', [a.card]).map(r => r.text);
      X('INSERT INTO templates(name,data) VALUES(?,?)', [clean(a.name, 80), JSON.stringify(c)]);
    },
    templates: () => Q('SELECT id,name FROM templates ORDER BY name'),
    del_template: a => X('DELETE FROM templates WHERE id=?', [a.id]),
    use_template: a => {
      const t = JSON.parse(Q('SELECT data FROM templates WHERE id=?', [a.id])[0].data);
      X('INSERT INTO cards(list_id,title,description,priority,pos,recur,fields) VALUES(?,?,?,?,?,?,?)', [a.list, t.title || 'Untitled', t.description || '', t.priority | 0, maxPos('cards', 'list_id', a.list), t.recur || '', t.fields || '']);
      const n = lastId(); (t.checklist || []).forEach((s, i) => X('INSERT INTO checklist_items(card_id,text,pos) VALUES(?,?,?)', [n, s, i]));
    },
    calendar: a => Q('SELECT c.id,c.title,c.due,c.done,c.priority,b.name AS board FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=0 AND c.due BETWEEN ? AND ? ORDER BY c.due', [a.from, a.to]),
    calendar_ms: a => Q('SELECT m.*,b.name AS board FROM milestones m JOIN boards b ON b.id=m.board_id WHERE m.due BETWEEN ? AND ? ORDER BY m.due', [a.from, a.to]),
    search: a => Q('SELECT c.id,c.title,c.due,b.id AS board_id,b.name AS board FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=0 AND (c.title LIKE ?1 OR c.description LIKE ?1 OR c.fields LIKE ?1) LIMIT 50', ['%' + String(a.text).replace(/%/g, '') + '%']),
    dashboard: a => {
      const base = 'SELECT c.id,c.title,c.due,b.id AS board_id,b.name AS board FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=0 AND c.done=0 AND c.due IS NOT NULL';
      const ids = (gs('recent') || '').split(',').map(Number).filter(Boolean);
      const rb = ids.length ? Q(`SELECT id AS board_id,name AS board FROM boards WHERE id IN (${ids.join(',')})`) : [];
      rb.sort((x, y) => ids.indexOf(x.board_id) - ids.indexOf(y.board_id));
      return {
        overdue: Q(base + ' AND c.due<? ORDER BY c.due', [a.today]), today: Q(base + ' AND c.due=? ORDER BY c.priority DESC', [a.today]), upcoming: Q(base + ' AND c.due>? ORDER BY c.due LIMIT 15', [a.today]),
        recent: Q('SELECT c.id,c.title,b.id AS board_id,b.name AS board FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=0 ORDER BY c.updated DESC LIMIT 8'),
        inbox: Q('SELECT * FROM inbox ORDER BY id DESC'),
        milestones: Q('SELECT m.*,b.name AS board,b.id AS board_id FROM milestones m JOIN boards b ON b.id=m.board_id WHERE m.done=0 ORDER BY m.due LIMIT 10'),
        favs: Q('SELECT id AS board_id,name AS board FROM boards WHERE fav=1 ORDER BY name'), recent_boards: rb,
        progress: Q('SELECT b.id AS board_id,b.name AS board,COUNT(c.id) AS total,COALESCE(SUM(c.done),0) AS done FROM boards b LEFT JOIN lists l ON l.board_id=b.id LEFT JOIN cards c ON c.list_id=l.id AND c.archived=0 GROUP BY b.id ORDER BY total DESC LIMIT 12')
      };
    },
    add_inbox: a => X('INSERT INTO inbox(text) VALUES(?)', [clean(a.text, 500)]),
    del_inbox: a => X('DELETE FROM inbox WHERE id=?', [a.id]),
    settings: () => Q('SELECT * FROM settings'),
    set_setting: a => ss(a.key, a.value),
    export_file: () => { const n = `project-manager-${stamp()}.json`; dl(n, JSON.stringify(dump(), null, 2), 'application/json'); return n; },
    export_text: a => { const n = String(a.name).replace(/[^\p{L}\p{N}._-]/gu, '_'); dl(n, a.text, 'text/markdown'); return n; },
    import_json: a => { let v; try { v = JSON.parse(a.text); } catch (e) { throw 'Invalid JSON: ' + e.message; } C.backup({}); load(v); },
    backup: () => {
      const n = `backup-${stamp()}.db`; store.set('bk_' + n, snap());
      store.keys().filter(k => k.startsWith('bk_')).sort().reverse().slice(5).forEach(k => store.del(k)); return n;
    },
    backups: () => store.keys().filter(k => k.startsWith('bk_')).map(k => k.slice(3)).sort().reverse(),
    restore: a => { const d = store.get('bk_' + a.name); if (!d) throw 'Unknown backup'; C.backup({}); open(unb64(d)); },
    open_data_folder: () => { throw NA; }, sync_set: () => { throw NA; }, sync_now: () => { throw NA; }, test_notify: () => { throw NA; },
    notify_text: a => { if (env.notify) env.notify(a.title, a.body); },
    seed: () => { SEED.forEach(([n, ic, col, bs]) => { X('INSERT INTO workspaces(name,color,icon) VALUES(?,?,?)', [n, col, ic]); const ws = lastId(); bs.forEach(([bn, ls]) => newBoard(ws, bn, '#1f2937', ls)); }); },
    archived: () => Q('SELECT c.id,c.title,c.due,b.id AS board_id,b.name AS board,l.name AS list FROM cards c JOIN lists l ON l.id=c.list_id JOIN boards b ON b.id=l.board_id WHERE c.archived=1 ORDER BY c.updated DESC LIMIT 300'),
    unarchive: a => X('UPDATE cards SET archived=0,updated=CURRENT_TIMESTAMP WHERE id=?', [a.id]),
    milestones: a => Q('SELECT * FROM milestones WHERE board_id=? ORDER BY due', [a.board]),
    add_milestone: a => { if (String(a.due || '').length !== 10) throw 'Pick a date'; X('INSERT INTO milestones(board_id,name,due) VALUES(?,?,?)', [a.board, clean(a.name, 100), a.due]); },
    toggle_milestone: a => X('UPDATE milestones SET done=1-done WHERE id=?', [a.id]),
    del_milestone: a => X('DELETE FROM milestones WHERE id=?', [a.id]),
    timeline: a => ({
      cards: Q("SELECT c.id,c.title,c.done,COALESCE(NULLIF(c.start,''),date(c.created)) AS start,c.due FROM cards c JOIN lists l ON l.id=c.list_id WHERE l.board_id=? AND c.archived=0 AND c.due IS NOT NULL ORDER BY start,c.due", [a.board]),
      milestones: Q('SELECT * FROM milestones WHERE board_id=? ORDER BY due', [a.board])
    }),
    filters: () => Q('SELECT * FROM filters ORDER BY name'),
    save_filter: a => X('INSERT INTO filters(name,data) VALUES(?,?)', [clean(a.name, 60), a.data]),
    del_filter: a => X('DELETE FROM filters WHERE id=?', [a.id]),
    set_list_color: a => X('UPDATE lists SET color=? WHERE id=?', [a.color, a.id]),
    set_list_pos: a => X('UPDATE lists SET pos=? WHERE id=?', [a.pos, a.id]),
    archive_cards: a => X('UPDATE cards SET archived=1,updated=CURRENT_TIMESTAMP WHERE list_id=? AND archived=0 AND (?=0 OR done=1)', [a.list, a.doneOnly ? 1 : 0]),
    toggle_fav: a => X('UPDATE boards SET fav=1-fav WHERE id=?', [a.id]),
    timer_toggle: a => X("UPDATE cards SET spent=spent+CASE WHEN timer>0 THEN CAST(strftime('%s','now') AS INTEGER)-timer ELSE 0 END,timer=CASE WHEN timer>0 THEN 0 ELSE CAST(strftime('%s','now') AS INTEGER) END WHERE id=?", [a.id]),
    touch_board: a => { const v = (gs('recent') || '').split(',').filter(s => s && s !== String(a.id)); v.unshift(String(a.id)); ss('recent', v.slice(0, 6).join(',')); },
    set_priority: a => X('UPDATE cards SET priority=?,updated=CURRENT_TIMESTAMP WHERE id=?', [Math.min(3, Math.max(0, a.priority | 0)), a.id]),
    set_due: a => X('UPDATE cards SET due=?,updated=CURRENT_TIMESTAMP WHERE id=?', [a.due || null, a.id]),
    ws_lists: a => Q('SELECT l.id,l.name AS list,b.name AS board FROM lists l JOIN boards b ON b.id=l.board_id WHERE b.workspace_id=? ORDER BY b.id,l.pos LIMIT 80', [a.ws])
  };

  function demoData() {
    const bid = n => Q('SELECT id FROM boards WHERE name=?', [n])[0].id;
    const lid = (b, n) => Q('SELECT id FROM lists WHERE board_id=? AND name=?', [bid(b), n])[0].id;
    let pos = 0;
    const card = (b, l, title, o) => {
      o = o || {};
      X(`INSERT INTO cards(list_id,title,description,due,priority,pos,done,recur) VALUES(?,?,?,${o.due ? `date('now','${o.due}')` : 'NULL'},?,?,?,?)`, [lid(b, l), title, o.desc || '', o.pri || 0, ++pos, o.done ? 1 : 0, o.recur || '']);
      const id = lastId();
      (o.checks || []).forEach(([t, d], i) => X('INSERT INTO checklist_items(card_id,text,done,pos) VALUES(?,?,?,?)', [id, t, d ? 1 : 0, i + 1]));
      (o.labels || []).forEach(n => { const r = Q('SELECT id FROM labels WHERE board_id=? AND name=?', [bid(b), n])[0]; if (r) X('INSERT OR IGNORE INTO card_labels VALUES(?,?)', [id, r.id]); });
      return id;
    };
    const tests = card('Projects', 'Backlog', 'Write tests for the sync module', { due: '-2 days', pri: 3, labels: ['Urgent', 'Bug'] });
    card('Projects', 'Backlog', 'Refactor the database layer', { labels: ['Idea'], checks: [['List slow queries', 1], ['Add indexes', 0]] });
    const rel = card('Projects', 'In progress', 'Ship v1.0 release', { due: '+7 days', pri: 2, labels: ['Urgent'], checks: [['Update changelog', 1], ['Build installer', 1], ['Write release notes', 0], ['Announce', 0]] });
    card('Projects', 'In progress', 'Polish the dark theme', { due: '+2 days', labels: ['Design'], desc: 'Check contrast on every view.' });
    card('Projects', 'Done', 'Set up CI pipeline', { done: 1 });
    X('INSERT INTO card_deps VALUES(?,?)', [rel, tests]);
    X('INSERT INTO milestones(board_id,name,due) VALUES(?,?,date(\'now\',\'+14 days\'))', [bid('Projects'), 'v1.0 release']);
    card('YouTube Pipeline', 'Ideas', '10 Rust tips for beginners', { labels: ['Content', 'Idea'] });
    card('YouTube Pipeline', 'Ideas', 'Desk setup tour', { labels: ['Content'] });
    card('YouTube Pipeline', 'Scripting', 'Walkthrough: building a Tauri app', { due: '+1 days', pri: 2, checks: [['Outline', 1], ['Draft script', 0], ['Record voice-over', 0]] });
    card('YouTube Pipeline', 'Scheduled', 'Weekly devlog upload', { due: '+5 days', recur: 'weekly', labels: ['Content'] });
    card('YouTube Pipeline', 'Published', 'Channel trailer', { done: 1 });
    X('INSERT INTO milestones(board_id,name,due) VALUES(?,?,date(\'now\',\'+30 days\'))', [bid('YouTube Pipeline'), 'Channel relaunch']);
    card('Game Dev', 'Concepts', 'Platformer prototype', { labels: ['Idea'] });
    card('Game Dev', 'In progress', 'Player controller', { pri: 2, checks: [['Movement', 1], ['Jump buffering', 0], ['Coyote time', 0]] });
    card('Game Dev', 'Testing', 'Playtest build 0.2', { due: '+6 days' });
    card('Learning Resources', 'Reading', 'OWASP Top 10', { labels: ['Research'] });
    card('Learning Resources', 'To read', 'Intro to threat modelling', { labels: ['Research'] });
    X('INSERT INTO inbox(text) VALUES(?)', ['Idea: add a habit tracker board']); X('INSERT INTO inbox(text) VALUES(?)', ['Renew the domain name']);
    ss('recent', [bid('YouTube Pipeline'), bid('Projects')].join(','));
    X("UPDATE boards SET fav=1 WHERE name='Projects'");
  }

  const saved = store.get('db');
  open(saved ? unb64(saved) : null);
  if (!Q('SELECT COUNT(*) AS n FROM workspaces')[0].n && !saved) { C.seed(); demoData(); save(); }
  const RO = new Set(['workspaces', 'boards', 'board', 'card_detail', 'templates', 'calendar', 'calendar_ms', 'search', 'dashboard', 'settings', 'backups', 'archived', 'milestones', 'timeline', 'filters', 'ws_lists', 'export_file', 'export_text', 'open_attachment', 'notify_text', 'backup']);
  return {
    commands: Object.keys(C),
    invoke: async (cmd, args) => { const f = C[cmd]; if (!f) throw 'Unknown command: ' + cmd; const r = f(args || {}); if (!RO.has(cmd)) save(); return r === undefined ? null : r; }
  };
}

if (typeof module !== 'undefined' && module.exports) { module.exports = { make }; return; }
if (typeof window === 'undefined' || window.__TAURI__) return;

// ---- browser bootstrap (web demo only)
const store = {
  get: k => { try { return localStorage.getItem('pm_' + k); } catch (e) { return null; } },
  set: (k, v) => { try { localStorage.setItem('pm_' + k, v); } catch (e) { console.warn('storage full'); } },
  del: k => { try { localStorage.removeItem('pm_' + k); } catch (e) {} },
  keys: () => { try { return Object.keys(localStorage).filter(k => k.startsWith('pm_')).map(k => k.slice(3)); } catch (e) { return []; } }
};
const env = {
  download: (name, text, mime) => { const a = document.createElement('a'); a.href = URL.createObjectURL(new Blob([text], { type: mime })); a.download = name; document.body.appendChild(a); a.click(); a.remove(); },
  openAtt: (name, data) => { const a = document.createElement('a'); a.href = URL.createObjectURL(new Blob([unb64(data)])); a.download = name; document.body.appendChild(a); a.click(); a.remove(); },
  notify: (t, b) => { if ('Notification' in window && Notification.permission === 'granted') new Notification(t, { body: b }); }
};
const ready = Promise.all([initSqlJs({ locateFile: f => 'vendor/' + f }), fetch('schema.sql').then(r => r.text())]).then(([SQL, schema]) => make(SQL, store, schema, env));
root.__TAURI__ = { core: { invoke: (c, a) => ready.then(b => b.invoke(c, a)) } };
root.__PM_DEMO__ = true;
document.addEventListener('DOMContentLoaded', () => {
  const bar = document.createElement('div');
  bar.style.cssText = 'position:fixed;right:12px;bottom:12px;z-index:9000;background:#171b24;color:#e6e9f0;border:1px solid #2a3142;border-radius:10px;padding:8px 12px;font:12px system-ui,sans-serif;display:flex;gap:10px;align-items:center;box-shadow:0 6px 20px #0006';
  bar.innerHTML = '<span>🧪 Web demo · data stays in this browser</span><a href="https://github.com/Sabinaya-Sapkota/project-manager/releases/latest" style="color:#6c8cff" target="_blank" rel="noopener">Get the desktop app</a><button id="pm-reset" style="background:none;border:1px solid #2a3142;color:inherit;border-radius:6px;padding:2px 8px;cursor:pointer">Reset demo</button>';
  document.body.appendChild(bar);
  document.getElementById('pm-reset').onclick = () => { if (confirm('Erase all demo data in this browser and start over?')) { store.keys().forEach(store.del); location.reload(); } };
});
if ('serviceWorker' in navigator && location.protocol.startsWith('http')) navigator.serviceWorker.register('sw.js').catch(() => {});
})(typeof window !== 'undefined' ? window : globalThis);
