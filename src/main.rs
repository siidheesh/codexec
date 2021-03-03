use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc::{channel, Receiver},
    thread::{self, spawn},
    time::Duration,
};
//static cmd: &str = r#"ls && sleep 3 && ls"#; //r#"docker build -t pytemp /home/siid/projects/codexec/container/docker/ && docker run -it --rm --name pytemp_inst pytemp"#;
static SCRIPT_NAME: &str = "script.py";
static DOCKERFILE_NAME: &str = "Dockerfile";
static DOCKERFILE: &str = r#"
FROM python:alpine

WORKDIR /usr/local
#COPY requirements.txt ./
#RUN pip install --no-cache-dir -r requirements.txt

COPY script.py /usr/local

CMD [ "python", "./script.py" ].."#;

static EXEC_TIMEOUT: u64 = 5;

pub enum Msg {
    Stdout(String),
    EOF,
    Error(String),
    Timeout, //Stderr(String),
}

/*
pub fn cleanup_jobs() {
    Command::new("docker")
        .args(&["rmi", "-f", &container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();
}*/

pub fn spawn_job(code: String, timeout_sec: u64) -> Receiver<Msg> {
    //Result<String, Job> {
    println!("in spawn_job: {} bytes of code", code.len());

    let rand_string: String = thread_rng()
        .sample_iter(&Alphanumeric)
        .take(20)
        .map(char::from)
        .flat_map(|c| c.to_lowercase())
        .collect::<String>();

    let container_name = "codex_".to_owned() + &rand_string;

    let temp_dir = format!("/tmp/{}/", container_name);

    fs::create_dir(&temp_dir).unwrap();

    let mut dockerfile_path = temp_dir.clone();
    dockerfile_path.push_str(DOCKERFILE_NAME);

    fs::write(&dockerfile_path, DOCKERFILE).unwrap();

    /*fs::copy(
        "/home/siid/projects/codexec/container/docker/script.py",
        &script_path,
    )
    .unwrap();*/

    let mut script_path = temp_dir.clone();
    script_path.push_str(SCRIPT_NAME);

    let mut script_file = File::create(script_path).unwrap();
    script_file.write_all(code.as_bytes()).unwrap();
    script_file.flush().unwrap();
    drop(script_file); // idk if script_file is automatically dropped in time or not

    Command::new("docker")
        .args(&["build", "-t", &container_name, &temp_dir])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .unwrap();

    //let mut lines = vec![];
    let (tx, rx) = channel();
    let tx_timer = tx.clone();

    let mut output = Command::new("docker")
        .args(&["run", "-it", "--rm", &container_name])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = output.stdout.take().unwrap();

    spawn(move || {
        //output.stdout.as_mut() {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let _ = if let Ok(_line) = line {
                tx.send(Msg::Stdout(_line))
            } else {
                tx.send(Msg::Error(format!("invalid line: {:?}", line)))
            };
        }

        let res = output.wait();

        if res.is_ok() {
            tx.send(Msg::EOF)
        } else {
            tx.send(Msg::Error(format!("cmd wasn't running: {:?}", res)))
        }
    });

    spawn(move || {
        thread::sleep(Duration::from_secs(timeout_sec));
        let _ = tx_timer.send(Msg::Timeout);
    });

    /*let receiver = spawn(move || -> Result<Vec<String>, Job> {
        for recv in rx {
            match recv {
                Msg::Stdout(line) => lines.push(line),
                Msg::EOF => return Ok(lines),
                Msg::Error(msg) => return Err(Job::Error(msg)),
                Msg::Timeout => return Err(Job::Timeout),
            }
        }
        Ok(lines)
    });

    let res = match receiver.join().unwrap() {
        Ok(lines) => Ok(lines.join("\n")),
        Err(e) => Err(e),
    };

    cleanup_job(&temp_dir, &container_name);

    res*/

    let (tx1, rx1) = channel();

    spawn(move || {
        for recv in rx {
            match &recv {
                Msg::EOF | Msg::Error(_) | Msg::Timeout => {
                    cleanup_job(&temp_dir, &container_name);
                    let _ = tx1.send(recv);
                    return; // drop tx1 and rx
                }
                _ => {
                    let _ = tx1.send(recv);
                }
            }
        }
    });

    rx1
}

#[inline]
pub fn cleanup_job(temp_dir: &str, container_name: &str) {
    //println!("in cleanup_job: {}", container_name);

    let _ = Command::new("docker")
        .args(&["rm", "-f", container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    let _ = Command::new("docker")
        .args(&["rmi", "-f", container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    let _ = fs::remove_dir_all(temp_dir);
}

use actix_web::{
    http::StatusCode,
    web::{self, Bytes},
    App, HttpRequest, HttpResponse, HttpServer, Responder,
};

async fn greet(req: HttpRequest) -> impl Responder {
    let name = req.match_info().get("name").unwrap_or("World");
    format!("Hello {}!", &name)
}

async fn handle_py(bytes: Bytes) -> Result<String, HttpResponse> {
    match String::from_utf8(bytes.to_vec()) {
        Ok(code) => {
            let mut lines = vec![];
            for recv in spawn_job(code, EXEC_TIMEOUT) {
                match recv {
                    Msg::Stdout(line) => lines.push(line),
                    Msg::EOF => {
                        return Ok(lines.join("\n"));
                    }
                    Msg::Error(msg) => {
                        return Err(HttpResponse::build(StatusCode::INTERNAL_SERVER_ERROR).body(msg))
                    }
                    Msg::Timeout => {
                        return Err(
                            HttpResponse::build(StatusCode::REQUEST_TIMEOUT).body(format!(
                                "codexec took longer than {} second(s)",
                                EXEC_TIMEOUT
                            )),
                        )
                    }
                }
            }
            Ok(lines.join("\n"))
        }
        Err(_) => Err(HttpResponse::build(StatusCode::BAD_REQUEST).finish()),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| {
        App::new()
            .route("/", web::get().to(greet))
            .route("/{name}", web::get().to(greet))
            .route("/py", web::post().to(handle_py))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
