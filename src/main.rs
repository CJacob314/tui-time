use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use chrono::Local;
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode},
    text::Line,
};
use tokio::io::unix::AsyncFd;
use tui_big_text::{BigText, PixelSize};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let tfd = every_minute_timerfd_create().context("timerfd_create failed")?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn event-listening thread
    let event_thread_handle = std::thread::spawn(move || -> anyhow::Result<()> {
        loop {
            if matches!(event::read()?, Event::Key(key_event) if key_event.code == KeyCode::Char('q'))
            {
                tx.send(())?;
                return Ok(());
            }
        }
    });

    let mut terminal = ratatui::init();
    loop {
        terminal.draw(draw)?;
        tokio::select! {
            _ = wait_then_consume_tfd_read(&tfd) => continue,
            _ = rx.recv() => {
                break;
            }
        }
    }
    ratatui::restore();
    event_thread_handle.join().unwrap()?;

    Ok(())
}

async fn wait_then_consume_tfd_read(tfd: &AsyncFd<OwnedFd>) -> anyhow::Result<()> {
    let mut guard = tfd.readable().await.context("tfd.readable failed")?;
    let mut buf = 0_u64;
    let ret = match unsafe { libc::read(tfd.as_raw_fd(), &raw mut buf as _, 8) } {
        ..0 => {
            let err = io::Error::last_os_error();

            // Check if this was from a discontinuous change to the kernel RT clock
            if err.raw_os_error() == Some(libc::ECANCELED) {
                // Clear readiness then re-arm
                guard.clear_ready();

                arm_tfd_to_every_minute(tfd).context("arm_tfd_to_every_minute failed")?;
                return Ok(());
            }

            Err(err)
        },
        0..8 => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short read on timer fd",
        )),
        8 => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "longer than 8 read on timer fd",
        )),
    };

    guard.clear_ready();

    Ok(ret?)
}

fn draw(frame: &mut Frame) {
    const TEXTHEIGHT: u16 = 5;
    let now = Local::now();

    let big_text = BigText::builder()
        .pixel_size(PixelSize::Full)
        .lines(&[Line::from(now.format("%I:%M %p").to_string())])
        .centered()
        .build();
    let mut area = frame.area();
    area.y = (area.height.saturating_sub(TEXTHEIGHT)) / 2;
    frame.render_widget(big_text, area);
}

fn every_minute_timerfd_create() -> anyhow::Result<AsyncFd<OwnedFd>> {
    let fd = unsafe {
        libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK)
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }

    let tfd = AsyncFd::new(unsafe { OwnedFd::from_raw_fd(fd) }).context("AsyncFd::new failed")?;

    arm_tfd_to_every_minute(&tfd).context("arm_tfd_to_every_minute call failed")?;

    Ok(tfd)
}

fn arm_tfd_to_every_minute(tfd: &impl AsRawFd) -> anyhow::Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("SystemTime::duration_since failed")?;

    let now_secs = now.as_secs();
    let next_minute_secs = (now_secs / 60 + 1) * 60;

    let new_itimerspec = libc::itimerspec {
        it_value: libc::timespec {
            tv_sec: next_minute_secs as libc::time_t,
            tv_nsec: 0,
        },
        it_interval: libc::timespec {
            tv_sec: 60,
            tv_nsec: 0,
        },
    };

    let flags = libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET;
    if unsafe {
        libc::timerfd_settime(
            tfd.as_raw_fd(),
            flags,
            &new_itimerspec,
            std::ptr::null_mut(),
        )
    } < 0
    {
        Err(io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}
