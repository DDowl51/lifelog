//! 用真实的进程跑一遍：`lifelogd` 连一个假的 niri，记下时段；`lifelog` 把它查出来。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

/// 先把两个程序编译出来，返回它们所在的目录。
fn build() -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = Command::new(&cargo)
        .args([
            "build",
            "--quiet",
            "-p",
            "lifelogd",
            "-p",
            "lifelog",
            "-p",
            "lifelog-mcp",
        ])
        .current_dir(&root)
        .status()
        .expect("运行 cargo build");
    assert!(status.success());

    let out = Command::new(&cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(&root)
        .output()
        .expect("运行 cargo metadata");
    let meta: Value = serde_json::from_slice(&out.stdout).unwrap();
    PathBuf::from(meta["target_directory"].as_str().unwrap()).join("debug")
}

struct World {
    bin: PathBuf,
    dir: tempfile::TempDir,
}

impl World {
    fn config(&self) -> PathBuf {
        self.dir.path().join("config.toml")
    }

    fn lifelog(&self, args: &[&str]) -> Output {
        Command::new(self.bin.join("lifelog"))
            // 不要连到用户真实的常驻进程
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .arg("--config")
            .arg(self.config())
            .args(args)
            .env_remove("NO_COLOR")
            .output()
            .expect("运行 lifelog")
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.lifelog(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("stdout 是一行 JSON")
    }

    /// 反复查询，直到结果满足条件。
    fn wait_for(&self, args: &[&str], ok: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let out = self.lifelog(args);
            if out.status.success()
                && let Ok(value) = serde_json::from_slice::<Value>(&out.stdout)
                && ok(&value)
            {
                return value;
            }
            assert!(Instant::now() < deadline, "等了 20 秒还没等到：{args:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// 测试结束（包括失败）时把子进程杀掉：不然它会占着输出管道，让整个测试命令挂住。
struct Guard(Child);

impl std::ops::Deref for Guard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Guard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn terminate(child: &mut Child) -> std::process::ExitStatus {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    child.wait().unwrap()
}

const SNAPSHOT: &str = r#"{"WindowsChanged":{"windows":[{"id":1,"title":"◐ nvim","app_id":"com.mitchellh.ghostty","is_focused":true},{"id":2,"title":"Rust 程序设计语言 — Zen Browser","app_id":"zen","is_focused":false}]}}"#;

#[test]
fn records_focus_and_reports_it() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    let socket = world.dir.path().join("niri.sock");
    let db = world.dir.path().join("data/lifelog.db");
    std::fs::write(
        world.config(),
        format!(
            "[components.wm-niri]\nsocket = {socket:?}\n\n\
             [components.storage-sqlite]\npath = {db:?}\n\n\
             [components.focus]\nflush_interval_secs = 1\n\n\
             # 测试环境不一定有 Wayland：停用空闲检测，和依赖它的 afk\n\
             [components.idle-wayland]\nenabled = false\n\n[components.vectors]\nenabled = false\n\n[components.embed-fastembed]\nenabled = false\n\n[components.notes]\nenabled = false\n\n[components.afk]\nenabled = false\n\n\
             # 测试里的时段只有一两秒\n\
             [components.query]\ntimeline_min_secs = 0\n"
        ),
    )
    .unwrap();

    // 常驻进程还没跑过：帮助照常，查询明确地失败
    let help = world.lifelog(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("today"));
    let early = world.lifelog(&["today"]);
    assert_eq!(early.status.code(), Some(69));
    assert!(early.stdout.is_empty());
    let error: Value = serde_json::from_slice(&early.stderr).unwrap();
    assert_eq!(error["error"]["kind"], "unavailable");

    // 假的 niri
    let listener = UnixListener::bind(&socket).unwrap();
    let mut daemon = Guard(
        Command::new(world.bin.join("lifelogd"))
            .env("XDG_RUNTIME_DIR", world.dir.path())
            .arg("--config")
            .arg(world.config())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 lifelogd"),
    );
    let (mut niri, _) = listener.accept().unwrap();
    let mut request = String::new();
    BufReader::new(niri.try_clone().unwrap())
        .read_line(&mut request)
        .unwrap();
    assert_eq!(request, "\"EventStream\"\n");
    writeln!(niri, "{SNAPSHOT}").unwrap();

    // 运行中就能查到，而且时段在变长（定时刷新）
    let first = world.wait_for(&["today"], |v| {
        v["rows"].as_array().is_some_and(|r| !r.is_empty())
    });
    assert_eq!(first["rows"][0]["key"], "com.mitchellh.ghostty");
    assert_eq!(first["by"], "app");
    world.wait_for(&["today"], |v| {
        v["total_ms"].as_i64() > first["total_ms"].as_i64().map(|t| t + 900)
    });

    // 切到另一个窗口，然后正常退出
    writeln!(niri, r#"{{"WindowFocusChanged":{{"id":2}}}}"#).unwrap();
    world.wait_for(&["today", "--by", "title"], |v| {
        v["rows"].as_array().is_some_and(|r| r.len() == 2)
    });
    std::thread::sleep(Duration::from_millis(300));
    let status = terminate(&mut daemon);
    assert!(status.success(), "收到 SIGTERM 应该正常退出：{status:?}");
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut daemon.stderr.take().unwrap(), &mut stderr).unwrap();
    assert_eq!(stderr, "", "正常运行不该有任何输出");

    // 停止之后：两个时段都在，标题已经规范化，退出时写下了最后一段
    let report = world.json(&["day", "today", "--by", "title"]);
    let mut titles: Vec<&str> = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    titles.sort_unstable();
    assert_eq!(titles, ["Rust 程序设计语言", "nvim"]);
    let zen = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["key"] == "Rust 程序设计语言")
        .unwrap();
    assert!(zen["total_ms"].as_i64().unwrap() >= 300, "{zen}");

    // 给人看的输出
    let text = world.lifelog(&["today", "--text"]);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains("（04:00 起）")
            && text.contains("com.mitchellh.ghostty")
            && text.contains('█'),
        "{text}"
    );
    assert!(!text.contains('\x1b'), "不是终端就不带颜色");

    // 时间线和周
    let timeline = world.json(&["timeline"]);
    let titles: Vec<&str> = timeline["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["title"].as_str())
        .collect();
    assert!(titles.contains(&"nvim"), "{timeline}");
    let week = world.json(&["week", "--by", "title"]);
    assert_eq!(week["days"].as_array().unwrap().len(), 7);
    assert!(week["total_ms"].as_i64().unwrap() > 0, "{week}");

    // 错误和退出码
    let bad = world.lifelog(&["day", "someday"]);
    assert_eq!(bad.status.code(), Some(65));
    assert!(bad.stdout.is_empty());
    assert_eq!(world.lifelog(&["nope"]).status.code(), Some(64));

    // --trace 能看到谁在调谁
    let traced = world.lifelog(&["today", "--trace"]);
    let trace = String::from_utf8(traced.stderr).unwrap();
    for expected in [
        r#""caller":"launcher","provider":"cli-clap","contract":"cli","method":"run""#,
        r#""caller":"cli-clap","provider":"query","contract":"cli.commands","method":"run""#,
        r#""caller":"query","provider":"activity","contract":"activity.read","method":"list""#,
        r#""caller":"activity","provider":"storage-sqlite","contract":"sql","method":"query""#,
    ] {
        assert!(trace.contains(expected), "追踪里没有 {expected}：\n{trace}");
    }
    assert!(
        serde_json::from_slice::<Value>(&traced.stdout).is_ok(),
        "追踪不污染 stdout"
    );
}

#[test]
fn windows_are_classified_and_redacted_before_storage() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    let socket = world.dir.path().join("niri.sock");
    let db = world.dir.path().join("lifelog.db");
    std::fs::write(
        world.config(),
        format!(
            "[components.wm-niri]\nsocket = {socket:?}\n\n\
             [components.storage-sqlite]\npath = {db:?}\n\n\
             [components.focus]\nflush_interval_secs = 1\n\n\
             [components.idle-wayland]\nenabled = false\n\n[components.vectors]\nenabled = false\n\n[components.embed-fastembed]\nenabled = false\n\n[components.notes]\nenabled = false\n\n[components.afk]\nenabled = false\n\n\
             [components.project]\nroots = [\"/srv/projects\"]\n\
             rules = [{{ workspace = '^work-(?<project>.+)$' }}]\n"
        ),
    )
    .unwrap();

    let listener = UnixListener::bind(&socket).unwrap();
    let mut daemon = Guard(
        Command::new(world.bin.join("lifelogd"))
            .env("XDG_RUNTIME_DIR", world.dir.path())
            .arg("--config")
            .arg(world.config())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 lifelogd"),
    );
    let (mut niri, _) = listener.accept().unwrap();
    BufReader::new(niri.try_clone().unwrap())
        .read_line(&mut String::new())
        .unwrap();
    writeln!(
        niri,
        r#"{{"WorkspacesChanged":{{"workspaces":[{{"id":5,"name":"work-station"}},{{"id":6,"name":null}}]}}}}"#
    )
    .unwrap();
    writeln!(
        niri,
        r#"{{"WindowsChanged":{{"windows":[{{"id":1,"title":"nvim /srv/projects/lifelog","app_id":"com.mitchellh.ghostty","is_focused":true,"workspace_id":6}},{{"id":2,"title":"银行 - KeePassXC","app_id":"org.keepassxc.KeePassXC","is_focused":false}},{{"id":3,"title":"✳ 气象站","app_id":"com.mitchellh.ghostty","is_focused":false,"workspace_id":5}}]}}}}"#
    )
    .unwrap();
    world.wait_for(&["today"], |v| {
        v["rows"].as_array().is_some_and(|r| !r.is_empty())
    });
    writeln!(niri, r#"{{"WindowFocusChanged":{{"id":2}}}}"#).unwrap();
    world.wait_for(&["today"], |v| {
        v["rows"].as_array().is_some_and(|r| r.len() == 2)
    });
    // 标题里没有路径的窗口，按所在工作区归类
    writeln!(niri, r#"{{"WindowFocusChanged":{{"id":3}}}}"#).unwrap();
    world.wait_for(&["today", "--by", "project"], |v| {
        v["rows"].as_array().is_some_and(|r| r.len() == 3)
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(terminate(&mut daemon).success());

    let keys = |by: &str| -> Vec<Option<String>> {
        let mut keys: Vec<Option<String>> = world.json(&["today", "--by", by])["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["key"].as_str().map(str::to_owned))
            .collect();
        keys.sort_unstable();
        keys
    };
    assert_eq!(
        keys("project"),
        [None, Some("lifelog".to_owned()), Some("station".to_owned())],
        "密码管理器那一段没有项目；任务名标题的窗口按工作区归类"
    );
    assert_eq!(
        keys("title"),
        [
            Some("[redacted]".to_owned()),
            Some("nvim /srv/projects/lifelog".to_owned()),
            Some("气象站".to_owned())
        ]
    );
    // 数据库文件和 WAL 里都找不到原标题
    for file in [db.clone(), db.with_extension("db-wal")] {
        let raw = std::fs::read(&file).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&raw).contains("银行"),
            "原标题不该进数据库：{}",
            file.display()
        );
    }
}

#[test]
fn shell_commands_are_imported_classified_and_redacted() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    let atuin = world.dir.path().join("atuin.db");
    let conn = rusqlite::Connection::open(&atuin).unwrap();
    conn.execute_batch(
        "CREATE TABLE history (
            id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL, duration INTEGER NOT NULL,
            exit INTEGER NOT NULL, command TEXT NOT NULL, cwd TEXT NOT NULL,
            session TEXT NOT NULL, hostname TEXT NOT NULL, deleted_at INTEGER,
            UNIQUE(timestamp, cwd, command));",
    )
    .unwrap();
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let now_ns = i64::try_from(now_ns).unwrap();
    let insert = |id: &str, ago_s: i64, command: &str, exit: i64| {
        conn.execute(
            "INSERT INTO history VALUES (?1, ?2, 2000000000, ?3, ?4, '/srv/projects/lifelog/src', 's', 'any', NULL)",
            rusqlite::params![id, now_ns - ago_s * 1_000_000_000, exit, command],
        )
        .unwrap();
    };
    insert("a", 30, "cargo test", 101);
    insert("b", 20, "export KEY=sk-abcdefghijklmnopqrstuvwx", 0);

    let socket = world.dir.path().join("niri.sock");
    std::fs::write(
        world.config(),
        format!(
            "[components.wm-niri]\nsocket = {socket:?}\n\n\
             [components.storage-sqlite]\npath = {:?}\n\n\
             [components.idle-wayland]\nenabled = false\n\n[components.vectors]\nenabled = false\n\n[components.embed-fastembed]\nenabled = false\n\n[components.notes]\nenabled = false\n\n[components.afk]\nenabled = false\n\n\
             [components.project]\nroots = [\"/srv/projects\"]\n\n\
             [components.shell-atuin]\npath = {atuin:?}\nall_hosts = true\n\n\
             [components.shell]\npoll_secs = 1\n",
            world.dir.path().join("lifelog.db")
        ),
    )
    .unwrap();
    let _listener = UnixListener::bind(&socket).unwrap();
    let mut daemon = Guard(
        Command::new(world.bin.join("lifelogd"))
            .env("XDG_RUNTIME_DIR", world.dir.path())
            .arg("--config")
            .arg(world.config())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 lifelogd"),
    );

    let commands = world.wait_for(&["commands"], |v| {
        v["entries"].as_array().is_some_and(|e| e.len() == 2)
    });
    // 后来执行的也会导入
    insert("c", 1, "git status", 0);
    world.wait_for(&["commands"], |v| {
        v["entries"].as_array().is_some_and(|e| e.len() == 3)
    });
    assert!(terminate(&mut daemon).success());

    let entries = commands["entries"].as_array().unwrap();
    assert_eq!(entries[0]["command"], "cargo test");
    assert_eq!(entries[0]["exit"], 101);
    assert_eq!(entries[0]["duration_ms"], 2000);
    assert_eq!(entries[0]["project"], "lifelog", "按工作目录归类");
    assert_eq!(entries[1]["command"], "export KEY=[redacted]");

    let text = world.lifelog(&["commands", "--text"]);
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("lifelog  cargo test  ✗101"), "{text}");
}

#[test]
fn browser_visits_are_imported_classified_and_redacted() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    let profile = world.dir.path().join("qute-profiles/lifelog/data");
    std::fs::create_dir_all(&profile).unwrap();
    let conn = rusqlite::Connection::open(profile.join("history.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE History (url NOT NULL, title NOT NULL, atime NOT NULL, redirect NOT NULL);",
    )
    .unwrap();
    let now_s = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    for (ago, url, title) in [
        (30, "https://docs.rs/jiff", "jiff - Rust"),
        (20, "https://bank.example/account", "余额"),
        (10, "about:blank", ""),
    ] {
        conn.execute(
            "INSERT INTO History VALUES (?1, ?2, ?3, 0)",
            rusqlite::params![url, title, now_s - ago],
        )
        .unwrap();
    }

    let socket = world.dir.path().join("niri.sock");
    std::fs::write(
        world.config(),
        format!(
            "[components.wm-niri]\nsocket = {socket:?}\n\n\
             [components.storage-sqlite]\npath = {:?}\n\n\
             [components.idle-wayland]\nenabled = false\n\n[components.vectors]\nenabled = false\n\n[components.embed-fastembed]\nenabled = false\n\n[components.notes]\nenabled = false\n\n[components.afk]\nenabled = false\n\n\
             [components.shell]\nenabled = false\n\n[components.shell-atuin]\nenabled = false\n\n\
             [components.browser-firefox]\ninstalls = []\n\n\
             [components.browser-chromium]\ninstalls = []\n\n\
             [components.browser-qutebrowser]\ndata_dir = {:?}\nbasedirs = [{:?}]\n\n\
             [components.browsing]\npoll_secs = 1\n\n\
             [components.search]\nindex_secs = 1\n\n\
             [components.project]\nrules = [{{ app = '^qutebrowser$', profile = '^(?<project>.+)$' }}]\n\n\
             [components.privacy]\nexclude = [{{ domain = \"bank.example\" }}]\n",
            world.dir.path().join("lifelog.db"),
            world.dir.path().join("no-main-instance"),
            world.dir.path().join("qute-profiles"),
        ),
    )
    .unwrap();
    let _listener = UnixListener::bind(&socket).unwrap();
    let mut daemon = Guard(
        Command::new(world.bin.join("lifelogd"))
            .env("XDG_RUNTIME_DIR", world.dir.path())
            .arg("--config")
            .arg(world.config())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 lifelogd"),
    );
    let visits = world.wait_for(&["visits"], |v| {
        v["entries"].as_array().is_some_and(|e| e.len() == 2)
    });
    // 常驻进程建索引，命令行检索
    let found = world.wait_for(&["search", "jiff", "rust"], |v| {
        v["hits"].as_array().is_some_and(|h| !h.is_empty())
    });
    assert_eq!(found["hits"][0]["url"], "https://docs.rs/jiff");
    let redacted = world.json(&["search", "余额"]);
    assert!(
        redacted["hits"].as_array().unwrap().is_empty(),
        "脱敏前的文字没进索引：{redacted}"
    );
    assert!(terminate(&mut daemon).success());

    let entries = visits["entries"].as_array().unwrap();
    assert_eq!(entries[0]["browser"], "qutebrowser");
    assert_eq!(entries[0]["title"], "jiff - Rust");
    assert_eq!(entries[0]["url"], "https://docs.rs/jiff");
    assert_eq!(entries[0]["project"], "lifelog", "按实例归到项目");
    assert_eq!(entries[1]["title"], "[redacted]", "排除的域名只留时刻");
    assert_eq!(entries[1]["url"], "[redacted]");
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut daemon.stderr.take().unwrap(), &mut stderr).unwrap();
    assert_eq!(stderr, "");
}

#[test]
fn agents_query_through_mcp() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    let socket = world.dir.path().join("niri.sock");
    std::fs::write(
        world.config(),
        format!(
            "[components.wm-niri]\nsocket = {socket:?}\n\n\
             [components.storage-sqlite]\npath = {:?}\n\n\
             [components.focus]\nflush_interval_secs = 1\n\n\
             [components.idle-wayland]\nenabled = false\n\n[components.vectors]\nenabled = false\n\n[components.embed-fastembed]\nenabled = false\n\n[components.notes]\nenabled = false\n\n[components.afk]\nenabled = false\n\n\
             [components.shell]\nenabled = false\n\n[components.browsing]\nenabled = false\n",
            world.dir.path().join("lifelog.db"),
        ),
    )
    .unwrap();

    // 先记下一段窗口
    let listener = UnixListener::bind(&socket).unwrap();
    let mut daemon = Guard(
        Command::new(world.bin.join("lifelogd"))
            .env("XDG_RUNTIME_DIR", world.dir.path())
            .arg("--config")
            .arg(world.config())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 lifelogd"),
    );
    let (mut niri, _) = listener.accept().unwrap();
    BufReader::new(niri.try_clone().unwrap())
        .read_line(&mut String::new())
        .unwrap();
    writeln!(niri, "{SNAPSHOT}").unwrap();
    world.wait_for(&["today"], |v| {
        v["total_ms"].as_i64().is_some_and(|t| t > 500)
    });
    assert!(terminate(&mut daemon).success());

    // 像 agent 的客户端那样说话
    let mut mcp = Command::new(world.bin.join("lifelog-mcp"))
        .env("XDG_RUNTIME_DIR", world.dir.path())
        .arg("--config")
        .arg(world.config())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 lifelog-mcp");
    {
        let mut stdin = mcp.stdin.take().unwrap();
        for line in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"lifelog_day","arguments":{"by":"title"}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"lifelog_search","arguments":{"query":"nvim"}}}"#,
        ] {
            writeln!(stdin, "{line}").unwrap();
        }
        // 关掉标准输入 = 客户端断开
    }
    let out = mcp.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 常驻进程已经停了：检索时向量那一路连不上，只按全文，并且说一声；除此之外不该有任何输出
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.lines().all(|l| l.contains("向量检索暂时用不了")),
        "{stderr}"
    );
    let replies: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("标准输出只有协议消息"))
        .collect();
    assert_eq!(replies.len(), 4, "通知不回：{replies:?}");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "lifelog-mcp");
    let tools: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"lifelog_timeline") && tools.contains(&"lifelog_search"),
        "{tools:?}"
    );
    assert_eq!(replies[2]["result"]["isError"], false);
    let day: Value =
        serde_json::from_str(replies[2]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(day["rows"][0]["key"], "nvim");
    // 常驻进程这次没开检索的定时器那么久：可能还没建索引，但调用本身要成功
    assert_eq!(replies[3]["result"]["isError"], false, "{}", replies[3]);
}

/// 一个只会回固定答案的模型接口，回 `times` 次。返回地址和收到的请求体。
fn fake_llm(times: usize) -> (String, std::sync::mpsc::Receiver<Value>) {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..times {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end().to_ascii_lowercase();
                if header.is_empty() {
                    break;
                }
                if let Some(v) = header.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            tx.send(serde_json::from_slice(&body).unwrap()).unwrap();
            let reply = r###"{"model":"fake-model","choices":[{"message":{"content":"## 概览\n写了 lifelog。"}}]}"###;
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });
    (url, rx)
}

#[test]
fn the_command_line_hands_summaries_to_the_daemon() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    let (llm, requests) = fake_llm(1);
    let notes = world.dir.path().join("notes");
    let socket = world.dir.path().join("niri.sock");
    std::fs::write(
        world.config(),
        format!(
            "[components.wm-niri]\nsocket = {socket:?}\n\n\
             [components.storage-sqlite]\npath = {:?}\n\n\
             [components.focus]\nflush_interval_secs = 1\n\n\
             [components.idle-wayland]\nenabled = false\n\n[components.vectors]\nenabled = false\n\n[components.embed-fastembed]\nenabled = false\n\n[components.notes]\nenabled = false\n\n[components.afk]\nenabled = false\n\n\
             [components.shell]\nenabled = false\n\n[components.browsing]\nenabled = false\n\n\
             [components.files-std]\nroot = {notes:?}\n\n\
             [components.llm-openai-compat]\nbase_url = {llm:?}\nmodel = \"fake-model\"\napi_key = \"k\"\n\n\
             [components.summary]\nmin_active_minutes = 0\n",
            world.dir.path().join("lifelog.db"),
        ),
    )
    .unwrap();

    let listener = UnixListener::bind(&socket).unwrap();
    let mut daemon = Guard(
        Command::new(world.bin.join("lifelogd"))
            .env("XDG_RUNTIME_DIR", world.dir.path())
            .arg("--config")
            .arg(world.config())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 lifelogd"),
    );
    let (mut niri, _) = listener.accept().unwrap();
    BufReader::new(niri.try_clone().unwrap())
        .read_line(&mut String::new())
        .unwrap();
    writeln!(niri, "{SNAPSHOT}").unwrap();
    let today = world.wait_for(&["today"], |v| {
        v["total_ms"].as_i64().is_some_and(|t| t > 500)
    });
    let date = today["date"].as_str().unwrap().to_owned();

    // 命令行进程只读；生成经 socket 交给常驻进程
    let result = world.json(&["summarize", "today"]);
    assert_eq!(result["days"][0]["outcome"], "written", "{result}");
    assert_eq!(result["days"][0]["model"], "fake-model");
    let path = notes.join(format!("daily/{date}.md"));
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(file.contains("## 概览\n写了 lifelog。"), "{file}");
    let sent = requests.recv().unwrap();
    let material = sent["messages"][1]["content"].as_str().unwrap();
    // 测试里的窗口只有几秒，时间线只列 5 分钟以上的段；按项目那一节总会有
    assert!(
        material.contains(&date) && material.contains("## 按项目"),
        "{material}"
    );

    // 手改过：不覆盖，也不调模型（假接口只回一次）
    std::fs::write(&path, file.replace("写了", "写完了")).unwrap();
    let kept = world.json(&["summarize", "today"]);
    assert_eq!(kept["days"][0]["outcome"], "kept_edited", "{kept}");

    // 常驻进程停了：明确地失败
    assert!(terminate(&mut daemon).success());
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut daemon.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(!stderr.contains("ERROR"), "{stderr}");
    let down = world.lifelog(&["summarize", "today"]);
    assert_eq!(down.status.code(), Some(69));
    assert!(
        String::from_utf8_lossy(&down.stderr).contains("没在运行"),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    assert!(
        !world.dir.path().join("lifelog/daemon.sock").exists(),
        "停止时删掉 socket"
    );
}

#[test]
fn a_typo_in_the_config_stops_both_programs() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    std::fs::write(
        world.config(),
        "[components.fcous]\nflush_interval_secs = 1\n",
    )
    .unwrap();

    let out = world.lifelog(&["today"]);
    assert_eq!(out.status.code(), Some(78));
    let message = String::from_utf8(out.stderr).unwrap();
    assert!(
        message.starts_with("lifelog: ") && message.contains("fcous") && message.contains("focus"),
        "{message}"
    );

    let out = Command::new(world.bin.join("lifelogd"))
        .env("XDG_RUNTIME_DIR", world.dir.path())
        .arg("--config")
        .arg(world.config())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78));
}

#[test]
fn the_cli_ignores_sections_of_components_it_does_not_run() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    // focus 和 wm-niri 只在常驻进程里；命令行读同一个文件时不能因此报错
    std::fs::write(
        world.config(),
        format!(
            "[components.focus]\nflush_interval_secs = 5\n\n[components.wm-niri]\nsocket = \"/nowhere\"\n\n\
             [components.storage-sqlite]\npath = {:?}\n",
            world.dir.path().join("x.db")
        ),
    )
    .unwrap();
    assert!(world.lifelog(&["--help"]).status.success());
}

#[test]
fn disabling_idle_alone_names_the_component_that_needs_it() {
    let world = World {
        bin: build(),
        dir: tempfile::tempdir().unwrap(),
    };
    std::fs::write(
        world.config(),
        format!(
            "[components.idle-wayland]\nenabled = false\n\n[components.vectors]\nenabled = false\n\n[components.embed-fastembed]\nenabled = false\n\n[components.notes]\nenabled = false\n\n[components.wm-niri]\nsocket = \"/nowhere\"\n\n\
             [components.storage-sqlite]\npath = {:?}\n",
            world.dir.path().join("x.db")
        ),
    )
    .unwrap();
    let out = Command::new(world.bin.join("lifelogd"))
        .env("XDG_RUNTIME_DIR", world.dir.path())
        .arg("--config")
        .arg(world.config())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(78));
    let message = String::from_utf8(out.stderr).unwrap();
    assert!(
        message.contains("`afk`") && message.contains("idle@1") && message.contains("停用 `afk`"),
        "{message}"
    );
}
