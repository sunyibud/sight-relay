use mac_capture::{Roi, capture_display, list_displays};
use std::{env, fs, path::PathBuf, process};

fn usage() {
    eprintln!("用法:\n  mac-capture list\n  mac-capture capture <display_id> <output.jpg> [x y width height]
  mac-capture upload <server_url> <device_id> <display_id> [x y width height]\n\n自动采集已由管理员关闭，请使用 Capture 客户端的 Option-A 快捷键手动采集。\nROI 使用 0..1 的相对坐标，默认全屏。首次运行需在系统设置中授予屏幕录制权限。");
}

fn main() {
    let _ = dotenvy::dotenv();
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("daemon") => {
            eprintln!("管理员已关闭自动采集功能，建议通过快捷键手动采集");
            process::exit(2);
        }
        Some("upload") => {
            let server = args.next().unwrap_or_else(|| {
                usage();
                process::exit(2)
            });
            let device = args.next().unwrap_or_else(|| {
                usage();
                process::exit(2)
            });
            let display_id = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                usage();
                process::exit(2)
            });
            let values: Vec<f32> = args
                .map(|v| v.parse())
                .collect::<Result<_, _>>()
                .unwrap_or_else(|_| {
                    usage();
                    process::exit(2);
                });
            let roi = match values.as_slice() {
                [] => Roi::full(),
                [x, y, w, h] => Roi {
                    x: *x,
                    y: *y,
                    width: *w,
                    height: *h,
                },
                _ => {
                    usage();
                    process::exit(2);
                }
            };
            let image = match capture_display(display_id, roi) {
                Ok(v) => v,
                Err(e) => fail(e),
            };
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(async move {
                let part = reqwest::multipart::Part::bytes(image.bytes)
                    .file_name("capture.jpg")
                    .mime_str("image/jpeg")
                    .expect("mime");
                let token = std::env::var("SIGHT_DEVICE_TOKEN")
                    .unwrap_or_else(|_| "local-dev-token".into());
                let form = reqwest::multipart::Form::new()
                    .text("token", token)
                    .text("device_id", device)
                    .part("image", part);
                let url = format!("{}/api/v1/captures", server.trim_end_matches('/'));
                let response = reqwest::Client::new()
                    .post(url)
                    .multipart(form)
                    .send()
                    .await
                    .expect("upload request");
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    eprintln!("上传失败 {status}: {body}");
                    process::exit(1);
                }
                println!("{body}");
            });
        }
        Some("list") => match list_displays() {
            Ok(displays) => {
                for (id, name, width, height) in displays {
                    println!("{id}\t{name}\t{width}x{height}");
                }
            }
            Err(err) => fail(err),
        },
        Some("capture") => {
            let display_id = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                usage();
                process::exit(2)
            });
            let output = PathBuf::from(args.next().unwrap_or_else(|| {
                usage();
                process::exit(2)
            }));
            let values: Vec<f32> = args
                .map(|v| v.parse())
                .collect::<Result<_, _>>()
                .unwrap_or_else(|_| {
                    usage();
                    process::exit(2)
                });
            let roi = match values.as_slice() {
                [] => Roi::full(),
                [x, y, width, height] => Roi {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                },
                _ => {
                    usage();
                    process::exit(2);
                }
            };
            match capture_display(display_id, roi).and_then(|image| {
                fs::write(&output, &image.bytes)
                    .map_err(|e| mac_capture::CaptureError::Screen(e.to_string()))
                    .map(|_| image)
            }) {
                Ok(image) => println!(
                    "saved={} bytes={} size={}x{} sha256={}",
                    output.display(),
                    image.bytes.len(),
                    image.width,
                    image.height,
                    image.sha256
                ),
                Err(err) => fail(err),
            }
        }
        None => {
            usage();
        }
        Some(_) => {
            usage();
            process::exit(2);
        }
    }
}

fn fail(error: mac_capture::CaptureError) -> ! {
    eprintln!("错误：{error}");
    process::exit(1)
}
