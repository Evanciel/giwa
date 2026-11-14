//! `giwa service` (SPEC §10·§12, ADR-0124). 로그인하면 서버가 뜨도록 사용자 권한으로 등록한다.
//!
//! 관리자 권한을 쓰지 않는다 (원칙 6). 리눅스는 systemd 사용자 단위, 맥은 launchd 에이전트,
//! 윈도우는 작업 스케줄러의 로그온 작업이다. 윈도우 서비스는 관리자 권한이 있어야 해서 쓰지 않는다.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use giwa_core::{Code, Error, Result};
use giwa_storage::DataDir;
use serde_json::{Value, json};

use crate::commands::{Out, open, running_server};
use crate::doctor::probe_giwa;
use crate::{Global, ServiceCmd, ServiceInstallArgs};

const START_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Manager {
    Systemd,
    Launchd,
    TaskScheduler,
}

impl Manager {
    fn current() -> Self {
        if cfg!(windows) {
            Self::TaskScheduler
        } else if cfg!(target_os = "macos") {
            Self::Launchd
        } else {
            Self::Systemd
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Systemd => "systemd-user",
            Self::Launchd => "launchd",
            Self::TaskScheduler => "task-scheduler",
        }
    }
}

/// 사이트마다 이름이 다르다. 같은 데이터 디렉터리면 언제나 같다.
fn service_name(data: &Path) -> String {
    let shown = data.display().to_string();
    let key = if cfg!(windows) {
        shown.to_lowercase()
    } else {
        shown
    };
    // FNV-1a. 비밀이 아니라 이름이라 버전이 바뀌어도 같은 값이면 된다.
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("giwa-{hash:016x}")
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// systemd 의 `ExecStart` 한 칸.
fn systemd_word(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{escaped}\"")
}

fn systemd_unit(program: &Path, args: &[String], data: &Path) -> String {
    let mut words = vec![systemd_word(&program.display().to_string())];
    words.extend(args.iter().map(|a| systemd_word(a)));
    format!(
        "[Unit]\nDescription=Giwa CMS ({})\n\n[Service]\nExecStart={}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        data.display().to_string().replace(['\r', '\n'], " "),
        words.join(" "),
    )
}

fn launchd_plist(label: &str, program: &Path, args: &[String], data: &Path) -> String {
    let mut words = vec![program.display().to_string()];
    words.extend(args.iter().cloned());
    let words: String = words
        .iter()
        .map(|w| format!("    <string>{}</string>\n", xml(w)))
        .collect();
    let log = xml(&data.join("service.log").display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{}</string>\n\
         \x20 <key>ProgramArguments</key>\n  <array>\n{words}  </array>\n\
         \x20 <key>RunAtLoad</key>\n  <true/>\n\
         \x20 <key>KeepAlive</key>\n  <dict><key>SuccessfulExit</key><false/></dict>\n\
         \x20 <key>StandardErrorPath</key>\n  <string>{log}</string>\n</dict>\n</plist>\n",
        xml(label),
    )
}

/// 윈도우 명령줄 한 칸. 파일 이름에는 `"` 가 없어 감싸기만 한다.
fn windows_word(s: &str) -> String {
    format!("\"{}\"", s.replace('"', ""))
}

/// `conhost --headless` 로 띄워 콘솔 창이 뜨지 않게 한다 (Windows 10 1809 이상).
fn task_xml(user: &str, program: &Path, args: &[String], data: &Path) -> String {
    let mut words = vec![
        "--headless".to_owned(),
        windows_word(&program.display().to_string()),
    ];
    words.extend(args.iter().map(|a| windows_word(a)));
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         \x20 <RegistrationInfo><Description>Giwa CMS</Description></RegistrationInfo>\n\
         \x20 <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>\n\
         \x20 <Principals><Principal id=\"Author\"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n\
         \x20 <Settings>\n\
         \x20   <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
         \x20   <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
         \x20   <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
         \x20   <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n\
         \x20   <RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>\n\
         \x20 </Settings>\n\
         \x20 <Actions Context=\"Author\"><Exec><Command>conhost.exe</Command><Arguments>{}</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>\n\
         </Task>\n",
        xml(&words.join(" ")),
        xml(&data.display().to_string()),
        user = xml(user),
    )
}

fn failed(detail: impl Into<String>) -> Error {
    Error::new(Code::External)
        .with_message("error.service")
        .arg("detail", detail.into())
}

/// 실행해서 (성공 여부, 합친 출력).
fn exec(program: &str, args: &[&str]) -> Result<(bool, String)> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| failed(format!("{program} 를 실행하지 못했습니다: {e}")))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok((out.status.success(), text.trim().to_owned()))
}

fn must(program: &str, args: &[&str]) -> Result<()> {
    let (ok, text) = exec(program, args)?;
    if ok {
        Ok(())
    } else {
        Err(failed(format!("{program} {}: {text}", args.join(" "))))
    }
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| failed("HOME 환경 변수가 없습니다"))
}

fn utf16_file(path: &Path, text: &str) -> Result<()> {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(path, bytes).map_err(Error::internal)
}

struct Paths {
    data: PathBuf,
    name: String,
    file: PathBuf,
}

fn paths(g: &Global, manager: Manager) -> Result<Paths> {
    let data = std::path::absolute(&g.data).map_err(Error::internal)?;
    let name = service_name(&data);
    let file = match manager {
        Manager::Systemd => home()?
            .join(".config/systemd/user")
            .join(format!("{name}.service")),
        Manager::Launchd => home()?
            .join("Library/LaunchAgents")
            .join(format!("{name}.plist")),
        Manager::TaskScheduler => data.join(format!("{name}.xml")),
    };
    Ok(Paths { data, name, file })
}

fn definition(manager: Manager, p: &Paths, program: &Path, args: &[String]) -> Result<String> {
    Ok(match manager {
        Manager::Systemd => systemd_unit(program, args, &p.data),
        Manager::Launchd => launchd_plist(&p.name, program, args, &p.data),
        Manager::TaskScheduler => {
            let (_, user) = exec("whoami", &[])?;
            task_xml(&user, program, args, &p.data)
        }
    })
}

fn launchd_target(name: &str) -> Result<(String, String)> {
    let (_, uid) = exec("id", &["-u"])?;
    Ok((format!("gui/{uid}"), format!("gui/{uid}/{name}")))
}

fn register(manager: Manager, p: &Paths, content: &str) -> Result<()> {
    if let Some(dir) = p.file.parent() {
        std::fs::create_dir_all(dir).map_err(Error::internal)?;
    }
    let file = p.file.display().to_string();
    match manager {
        Manager::Systemd => {
            std::fs::write(&p.file, content).map_err(Error::internal)?;
            must("systemctl", &["--user", "daemon-reload"])?;
            must("systemctl", &["--user", "enable", "--now", &p.name])?;
            must("systemctl", &["--user", "restart", &p.name])
        }
        Manager::Launchd => {
            std::fs::write(&p.file, content).map_err(Error::internal)?;
            let (domain, target) = launchd_target(&p.name)?;
            // 이미 있으면 내리고 새 정의로 올린다. 없어서 실패하는 것은 괜찮다.
            let _ = exec("launchctl", &["bootout", &target]);
            must("launchctl", &["bootstrap", &domain, &file])
        }
        Manager::TaskScheduler => {
            utf16_file(&p.file, content)?;
            let made = must(
                "schtasks",
                &["/Create", "/XML", &file, "/TN", &p.name, "/F"],
            );
            let _ = std::fs::remove_file(&p.file);
            made?;
            must("schtasks", &["/Run", "/TN", &p.name])
        }
    }
}

fn registered(manager: Manager, p: &Paths) -> Result<bool> {
    Ok(match manager {
        Manager::Systemd | Manager::Launchd => p.file.is_file(),
        Manager::TaskScheduler => exec("schtasks", &["/Query", "/TN", &p.name])?.0,
    })
}

fn unregister(manager: Manager, p: &Paths) -> Result<()> {
    match manager {
        Manager::Systemd => {
            let _ = exec("systemctl", &["--user", "disable", "--now", &p.name]);
            std::fs::remove_file(&p.file).map_err(Error::internal)?;
            let _ = exec("systemctl", &["--user", "daemon-reload"]);
            Ok(())
        }
        Manager::Launchd => {
            let (_, target) = launchd_target(&p.name)?;
            let _ = exec("launchctl", &["bootout", &target]);
            std::fs::remove_file(&p.file).map_err(Error::internal)
        }
        Manager::TaskScheduler => {
            let _ = exec("schtasks", &["/End", "/TN", &p.name]);
            must("schtasks", &["/Delete", "/TN", &p.name, "/F"])
        }
    }
}

/// 주소에서 답하는 쪽. `None` 이면 아무도 듣지 않는다.
async fn answering(bind: &str) -> Option<bool> {
    probe_giwa(bind.parse().ok()?).await
}

fn port_taken(bind: &str) -> Error {
    Error::new(Code::PortInUse)
        .with_message("error.port_in_use")
        .arg("port", bind.rsplit(':').next().unwrap_or(bind))
        .next("giwa service install --bind 127.0.0.1:<다른 포트>")
}

async fn install(g: &Global, args: ServiceInstallArgs) -> Out {
    let dir = DataDir::new(&g.data);
    if !dir.is_initialized() {
        return Err(
            failed("초기화된 데이터 디렉터리가 아닙니다").next("giwa init --preset blog --yes")
        );
    }
    let (_ctx, config, db) = open(g).await?;
    db.close().await;
    let bind = args.bind.clone().unwrap_or_else(|| config.bind.clone());
    if !bind
        .parse::<std::net::SocketAddr>()
        .is_ok_and(|a| a.port() != 0)
    {
        return Err(Error::new(Code::Validation)
            .with_message("error.usage_detail")
            .arg(
                "detail",
                format!("서비스는 정해진 주소가 필요합니다: {bind}"),
            )
            .next("giwa service install --bind 127.0.0.1:8080"));
    }
    let manager = Manager::current();
    let p = paths(g, manager)?;
    let program = std::env::current_exe().map_err(Error::internal)?;
    let words = vec![
        "--data".to_owned(),
        p.data.display().to_string(),
        "serve".to_owned(),
        "--bind".to_owned(),
        bind.clone(),
    ];
    if args.print {
        let content = definition(manager, &p, &program, &words)?;
        let data = json!({
            "manager": manager.id(),
            "name": p.name,
            "file": p.file.display().to_string(),
            "content": content,
            "registered": false,
            "printed": true,
        });
        return Ok((data, content));
    }

    let before = answering(&bind).await;
    if before == Some(false) {
        return Err(port_taken(&bind));
    }
    // 이미 도는 서버가 이 사이트면 그대로 둔다. 다음 로그인부터 서비스가 띄운다.
    if before == Some(true) && !same_site(&dir, &bind) {
        return Err(port_taken(&bind));
    }
    let already = before == Some(true);
    let content = definition(manager, &p, &program, &words)?;
    // 등록이 막혀도 사이트는 띄울 수 있다.
    register(manager, &p, &content).map_err(|e| e.next(format!("giwa serve --bind {bind}")))?;

    let url = format!("http://{bind}");
    let waited = tokio::time::Instant::now();
    while answering(&bind).await != Some(true) {
        if waited.elapsed() > START_WAIT {
            return Err(failed("등록했지만 서버가 뜨지 않았습니다")
                .next("giwa doctor")
                .next("giwa service uninstall"));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let human = format!(
        "등록했습니다: {} ({})\n주소: {url}\n로그인하면 자동으로 뜹니다.{}",
        p.name,
        manager.id(),
        if manager == Manager::Systemd {
            "\n로그인 전에도 뜨게 하려면: loginctl enable-linger"
        } else {
            ""
        }
    );
    let data = json!({
        "manager": manager.id(),
        "name": p.name,
        "registered": true,
        "running": true,
        "already_running": already,
        "url": url,
    });
    Ok((data, human))
}

/// 그 주소의 giwa 가 이 데이터 디렉터리를 쓰는가. `server.json` 은 서버가 켤 때 적는다.
fn same_site(dir: &DataDir, bind: &str) -> bool {
    running_server(dir).is_some_and(|(addr, _)| addr == bind)
}

async fn uninstall(g: &Global) -> Out {
    let manager = Manager::current();
    let p = paths(g, manager)?;
    let was = registered(manager, &p)?;
    if was {
        unregister(manager, &p)?;
        stop_leftover(&DataDir::new(&g.data)).await;
    }
    let data = json!({ "manager": manager.id(), "name": p.name, "removed": was });
    let human = if was {
        format!("등록을 지웠습니다: {}", p.name)
    } else {
        "등록된 서비스가 없습니다".to_owned()
    };
    Ok((data, human))
}

/// 작업을 끝내도 `conhost` 밑의 서버가 남을 수 있다. 윈도우에서 그 서버만 끈다.
async fn stop_leftover(dir: &DataDir) {
    if !cfg!(windows) {
        return;
    }
    let Some((bind, pid)) = running_server(dir) else {
        return;
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    if answering(&bind).await == Some(true) {
        let _ = exec("taskkill", &["/PID", &pid.to_string(), "/F"]);
    }
}

async fn status(g: &Global) -> Out {
    let manager = Manager::current();
    let p = paths(g, manager)?;
    let is_registered = registered(manager, &p).unwrap_or(false);
    let dir = DataDir::new(&g.data);
    let bind = running_server(&dir).map(|(bind, _)| bind);
    let running = match &bind {
        Some(b) => answering(b).await == Some(true),
        None => false,
    };
    let url = bind.filter(|_| running).map(|b| format!("http://{b}"));
    let human = format!(
        "{} ({}): {}, 서버 {}",
        p.name,
        manager.id(),
        if is_registered {
            "등록됨"
        } else {
            "등록 안 됨"
        },
        url.as_deref().unwrap_or("꺼짐"),
    );
    let data: Value = json!({
        "manager": manager.id(),
        "name": p.name,
        "registered": is_registered,
        "running": running,
        "url": url,
    });
    Ok((data, human))
}

pub(crate) fn is_registered(g: &Global) -> Result<bool> {
    let manager = Manager::current();
    registered(manager, &paths(g, manager)?)
}

pub(crate) async fn run(g: &Global, cmd: ServiceCmd) -> Out {
    match cmd {
        ServiceCmd::Install(args) => install(g, args).await,
        ServiceCmd::Uninstall => uninstall(g).await,
        ServiceCmd::Status => status(g).await,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn args() -> Vec<String> {
        [
            "--data",
            "/집/사이트 & 하나",
            "serve",
            "--bind",
            "127.0.0.1:8080",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    #[test]
    fn 이름은_사이트마다_다르고_늘_같다() {
        let a = service_name(Path::new("/srv/a"));
        assert_eq!(a, service_name(Path::new("/srv/a")));
        assert_ne!(a, service_name(Path::new("/srv/b")));
        assert!(a.starts_with("giwa-") && a.len() == 21, "{a}");
    }

    #[test]
    fn systemd_단위는_칸을_감싸고_특수_문자를_막는다() {
        let unit = systemd_unit(
            Path::new("/opt/giwa 앱/giwa"),
            &["--data".to_owned(), "/a%b$c\"d".to_owned()],
            Path::new("/a"),
        );
        assert!(
            unit.contains(r#"ExecStart="/opt/giwa 앱/giwa" "--data" "/a%%b$$c\"d""#),
            "{unit}"
        );
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn plist_는_값을_이스케이프한다() {
        let plist = launchd_plist("maru-x", Path::new("/opt/giwa"), &args(), Path::new("/a"));
        assert!(
            plist.contains("<string>/집/사이트 &amp; 하나</string>"),
            "{plist}"
        );
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn 작업_정의는_사용자_권한이고_창을_띄우지_않는다() {
        let task = task_xml(
            r"PC\나",
            Path::new(r"C:\앱\giwa.exe"),
            &args(),
            Path::new(r"C:\a&b"),
        );
        assert!(
            task.contains("<RunLevel>LeastPrivilege</RunLevel>"),
            "{task}"
        );
        assert!(task.contains(r"<UserId>PC\나</UserId>"));
        assert!(task.contains("<Command>conhost.exe</Command>"));
        assert!(
            task.contains("--headless &quot;C:\\앱\\giwa.exe&quot;"),
            "{task}"
        );
        assert!(task.contains("<WorkingDirectory>C:\\a&amp;b</WorkingDirectory>"));
    }
}
