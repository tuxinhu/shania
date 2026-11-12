use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::settings::settings_dir;

/// 会话目录：`~/.shania/sessions/`，一个会话一个文件（`<id>.json`）。
fn sessions_dir() -> Result<PathBuf, String> {
    let dir = settings_dir()?.join("sessions");
    fs::create_dir_all(&dir).map_err(|e| format!("创建会话目录失败：{e}"))?;
    Ok(dir)
}

/* ------------------------------------------------------------------ */
/* 文件名                                                               */
/* ------------------------------------------------------------------ */

/// 会话 id 落到文件名。id 来自前端（uuid），这里仍然过滤一遍，
/// 避免出现 `../` 之类的东西把文件写到目录外面。
fn safe_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned.chars().take(80).collect()
    }
}

fn session_file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.json", safe_id(id)))
}

/// 只认 `<id>.json`，跳过旧聚合文件、备份与临时文件
fn is_session_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name == "sessions.json" {
        return false;
    }
    if name.ends_with(".bak") || name.ends_with(".json.tmp") {
        return false;
    }
    name.ends_with(".json")
}

fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, content.as_bytes()).map_err(|e| format!("写入临时文件失败：{e}"))?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| format!("替换旧会话文件失败：{e}"))?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("保存会话失败：{e}"))?;
    Ok(())
}

/* ------------------------------------------------------------------ */
/* 读写                                                                 */
/* ------------------------------------------------------------------ */

/// 读出目录里所有会话，按更新时间倒序（前端还会自己再排序，这里只是让结果稳定）。
fn read_session_files(dir: &Path) -> Vec<Value> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut out: Vec<Value> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_session_file(&path) {
            continue;
        }
        // 单个文件坏掉不该让整段历史都读不出来，跳过并留个痕迹即可
        match fs::read_to_string(&path).and_then(|text| {
            serde_json::from_str::<Value>(&text).map_err(|e| std::io::Error::other(e.to_string()))
        }) {
            Ok(value) if value.is_object() => out.push(value),
            Ok(_) => shania_core::log_warn!("忽略非对象会话文件：{}", path.display()),
            Err(err) => shania_core::log_warn!("跳过无法解析的会话文件 {}：{err}", path.display()),
        }
    }

    out.sort_by(|a, b| {
        let ts = |v: &Value| v.get("updatedAt").and_then(|x| x.as_i64()).unwrap_or(0);
        ts(b).cmp(&ts(a))
    });
    out
}

/// 把整份数组同步成「一会话一文件」：
/// 内容变了才写，数组里没有的会话文件删掉，读不懂的文件原样留着。
fn sync_session_files(dir: &Path, json: &str) -> Result<usize, String> {
    let list: Vec<Value> =
        serde_json::from_str(json).map_err(|e| format!("会话数据格式错误：{e}"))?;

    let mut keep: Vec<String> = Vec::with_capacity(list.len());
    let mut written = 0usize;

    for session in &list {
        let id = session
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "会话数据缺少 id".to_string())?;
        let file_id = safe_id(id);
        let path = session_file(dir, id);
        keep.push(file_id);

        let text =
            serde_json::to_string(session).map_err(|e| format!("序列化会话失败：{e}"))?;
        // 内容没变就别动磁盘——一次保存通常只有一条会话真的变了
        if fs::read_to_string(&path).map(|old| old == text).unwrap_or(false) {
            continue;
        }
        write_atomic(&path, &text)?;
        written += 1;
    }

    // 清理被删除的会话。解析失败的文件不在 keep 里，但也不删——宁可留垃圾，不误删数据
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_session_file(&path) {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if keep.contains(&stem) {
                continue;
            }
            // 只有能读出 id 的文件才删，且确认这个 id 确实不在新列表里
            let removable = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|v| v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string()))
                .map(|id| !list.iter().any(|s| s.get("id").and_then(|v| v.as_str()) == Some(id.as_str())))
                .unwrap_or(false);
            if removable {
                let _ = fs::remove_file(&path);
            }
        }
    }

    Ok(written)
}

/* ------------------------------------------------------------------ */
/* 旧格式迁移（整份数组 → 每会话一个文件）                                 */
/* ------------------------------------------------------------------ */

/// 历史位置，按新→旧：会话目录里的聚合文件、`~/.shania/`、更早的 app_data_dir
fn legacy_aggregates(app: &AppHandle, dir: &Path) -> Vec<PathBuf> {
    let mut paths = vec![dir.join("sessions.json")];
    if let Ok(base) = settings_dir() {
        paths.push(base.join("sessions.json"));
    }
    if let Ok(appdata) = app.path().app_data_dir() {
        paths.push(appdata.join("sessions.json"));
    }
    paths.retain(|p| p.exists());
    paths
}

/// 把聚合文件拆成单会话文件，成功后原文件改名 `.bak`。返回导入的会话数。
fn import_aggregate(file: &Path, dir: &Path) -> Option<usize> {
    let text = fs::read_to_string(file).ok()?;
    let list: Vec<Value> = serde_json::from_str(&text).ok()?;

    let mut count = 0usize;
    for session in &list {
        let Some(id) = session.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let path = session_file(dir, id);
        // 已经有同名文件说明这条会话更新一些，不覆盖
        if path.exists() {
            continue;
        }
        if let Ok(one) = serde_json::to_string(session) {
            if write_atomic(&path, &one).is_ok() {
                count += 1;
            }
        }
    }

    if let Some(name) = file.file_name().and_then(|n| n.to_str()) {
        let _ = fs::rename(file, file.with_file_name(format!("{name}.bak")));
    }
    Some(count)
}

/// 依次尝试历史位置，第一个成功的算数（与 Tauri 无关的部分已抽出，便于单测）
fn import_first_of(candidates: &[PathBuf], dir: &Path) -> Option<usize> {
    candidates
        .iter()
        .find_map(|file| import_aggregate(file, dir))
}

fn migrate_legacy(app: &AppHandle, dir: &Path) {
    let candidates = legacy_aggregates(app, dir);
    if candidates.is_empty() {
        return;
    }
    import_first_of(&candidates, dir);
}

/* ------------------------------------------------------------------ */
/* 命令                                                                 */
/* ------------------------------------------------------------------ */

#[tauri::command]
pub fn load_sessions(app: AppHandle) -> Result<String, String> {
    let dir = sessions_dir()?;
    // 老版本存的是一个聚合数组，先拆开再读
    migrate_legacy(&app, &dir);

    let list = read_session_files(&dir);
    serde_json::to_string(&list).map_err(|e| format!("读取会话失败：{e}"))
}

#[tauri::command]
pub fn save_sessions(app: AppHandle, json: String) -> Result<(), String> {
    let dir = sessions_dir()?;
    // 万一在读取之前就发生保存，先把旧聚合文件拆进来，避免它下次启动"复活"
    migrate_legacy(&app, &dir);

    sync_session_files(&dir, &json).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shania-storage-test-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录失败");
        dir
    }

    fn session(id: &str, title: &str, updated: i64) -> Value {
        serde_json::json!({
            "id": id,
            "title": title,
            "messages": [],
            "createdAt": updated,
            "updatedAt": updated
        })
    }

    #[test]
    fn saves_one_file_per_session() {
        let dir = temp_dir("split");
        let json = serde_json::json!([session("a1", "会话一", 2), session("b2", "会话二", 1)]).to_string();

        let written = sync_session_files(&dir, &json).unwrap();
        assert_eq!(written, 2);
        assert!(dir.join("a1.json").exists());
        assert!(dir.join("b2.json").exists());

        // 内容未变时不重复写盘
        assert_eq!(sync_session_files(&dir, &json).unwrap(), 0);

        let loaded = read_session_files(&dir);
        assert_eq!(loaded.len(), 2);
        // 按 updatedAt 倒序
        assert_eq!(loaded[0]["id"], "a1");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deletes_removed_sessions_only() {
        let dir = temp_dir("prune");
        let two = serde_json::json!([session("a1", "一", 1), session("b2", "二", 1)]).to_string();
        sync_session_files(&dir, &two).unwrap();

        // 一个文件故意写坏，模拟磁盘异常
        let broken = dir.join("c3.json");
        fs::write(&broken, "{ 坏掉的 JSON").unwrap();

        let one = serde_json::json!([session("a1", "一", 2)]).to_string();
        sync_session_files(&dir, &one).unwrap();

        assert!(dir.join("a1.json").exists());
        assert!(!dir.join("b2.json").exists(), "被删掉的会话文件应当清理");
        assert!(broken.exists(), "读不懂的文件不能被误删");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn splits_legacy_aggregate_and_keeps_backup() {
        let dir = temp_dir("import");
        let aggregate = dir.join("sessions.json");
        let json = serde_json::json!([session("a1", "旧会话一", 5), session("b2", "旧会话二", 4)]).to_string();
        fs::write(&aggregate, &json).unwrap();

        let count = import_first_of(&[aggregate.clone()], &dir).expect("应当导入成功");
        assert_eq!(count, 2);
        assert!(dir.join("a1.json").exists());
        assert!(dir.join("b2.json").exists());
        assert!(!aggregate.exists());
        assert!(dir.join("sessions.json.bak").exists());

        let loaded = read_session_files(&dir);
        assert_eq!(loaded.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_does_not_overwrite_newer_file() {
        let dir = temp_dir("nooverwrite");
        fs::write(dir.join("a1.json"), session("a1", "新标题", 9).to_string()).unwrap();
        let aggregate = dir.join("sessions.json");
        fs::write(&aggregate, serde_json::json!([session("a1", "旧标题", 1)]).to_string()).unwrap();

        import_first_of(&[aggregate], &dir).unwrap();

        let text = fs::read_to_string(dir.join("a1.json")).unwrap();
        assert!(text.contains("新标题"), "已存在的单会话文件不应被聚合文件覆盖");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn falls_through_to_older_location() {
        let dir = temp_dir("chain");
        let missing = dir.join("prev.json");
        let appdata = dir.join("appdata.json");
        fs::write(&appdata, serde_json::json!([session("z9", "更早的会话", 1)]).to_string()).unwrap();

        let sessions = dir.join("sessions");
        fs::create_dir_all(&sessions).unwrap();

        let count = import_first_of(&[missing, appdata.clone()], &sessions).expect("应当找到更早的位置");
        assert_eq!(count, 1);
        assert!(sessions.join("z9.json").exists());
        assert!(!appdata.exists());
        assert!(dir.join("appdata.json.bak").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitizes_dangerous_ids() {
        // `../../` 共 6 个字符，各自换成下划线，路径分隔符不会漏出去
        assert_eq!(safe_id("../../etc/passwd"), "______etc_passwd");
        assert_eq!(safe_id("normal-uuid_1"), "normal-uuid_1");
        assert_eq!(safe_id(""), "session");
    }
}
