#![feature(async_closure)]

use futures::channel::mpsc::unbounded;
use quicli::prelude::*;
use std::{iter::repeat, path::PathBuf};
use structopt::StructOpt;

use tokio::io;

#[derive(Debug, StructOpt)]
#[structopt(name = "wtp-downloader", about = "wtp-downloader HELP Menu.")]
struct Cli {
    /// Character device, use "-" for stdio.
    #[structopt(short, long)]
    char: PathBuf,
    #[structopt(short, long)]
    BTIM: PathBuf,
    #[structopt(short, long)]
    Image_h: Vec<PathBuf>,
}

async fn get_current<F: tokio::io::AsyncSeek + Unpin>(f: &mut F) -> Result<u64, std::io::Error> {
    use std::io::SeekFrom;
    use tokio::io::AsyncSeekExt;
    f.seek(SeekFrom::Current(0)).await
}

async fn get_file_len<F: tokio::io::AsyncSeek + Unpin>(f: &mut F) -> Result<u64, std::io::Error> {
    use std::io::SeekFrom;
    use tokio::io::AsyncSeekExt;

    let old_pos = get_current(f).await?;
    let len = f.seek(SeekFrom::End(0)).await?;

    // Avoid seeking a third time when we were already at the end of the
    // stream. The branch is usually way cheaper than a seek operation.
    if old_pos != len {
        f.seek(SeekFrom::Start(old_pos)).await?;
    }

    Ok(len)
}

#[tokio::main]
async fn main() -> CliResult {
    use rexpect::ReadUntil;
    use std::convert::TryInto;
    let args = Cli::from_args();
    use tokio::io::AsyncReadExt;

    println!("{:#?}", args);

    let (char_in, char_out): (
        Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) = if args.char == PathBuf::from("-") {
        (Box::new(tokio::io::stdin()), Box::new(tokio::io::stdout()))
    } else {
        let mut settings: tokio_serial::SerialPortSettings = Default::default();
        settings.baud_rate = 115200;
        let mut serial = tokio_serial::Serial::from_path(args.char, &settings).unwrap();
        serial.set_exclusive(true)?;
        let split = io::split(tokio_compat_02::IoCompat::new(serial));
        (Box::new(split.0), Box::new(split.1))
    };

    let (mut btim, mut image_h) = {
        use futures::stream::{iter as stream_iter, StreamExt};
        use tokio::{fs::File, io::BufReader};

        let mut btim = File::open(args.BTIM).await?;
        let btim_len = get_file_len(&mut btim).await?;
        let btim = BufReader::new(btim);
        let images_h = stream_iter(args.Image_h)
            .then(async move |x| {
                let mut image_h = File::open(x).await.unwrap();
                let image_h_len = get_file_len(&mut image_h).await.unwrap();
                (BufReader::new(image_h), image_h_len)
            })
            .collect::<Vec<_>>()
            .await;

        ((btim, btim_len), images_h)
    };

    let mut session = rexpect::_async::spawn_async_stream(
        tokio_util::io::ReaderStream::new(char_in),
        char_out,
        Some(10),
    )
    .await;

    session.send("wtp\r").await.expect("Sending wtp");
    session.exp_string("wtp\r\n").await.expect("Receive wtp");
    session
        .send_bytes(&[0u8, 0o323, 2, b'+'])
        .await
        .expect("Sending preamble");
    session
        .exp_bytes(vec![0u8, 0o323, 2, b'+'])
        .await
        .expect("Receive preamble");
    session.send("+\0\0\0\0\0\0\0").await.unwrap();
    session.exp_string("+\0\0\x01\0\0").await.unwrap();
    session
        .send_bytes(&[b' ', 0, 0, 0o10, 0, 0, 0, 0])
        .await
        .expect("Sending GetVersion");
    session
        .exp_bytes(vec![b' ', 0, 0, 0, 0o10, 0o24])
        .await
        .unwrap();
    let mut version = session
        .read_bytes_until(&ReadUntil::NBytes(4))
        .await
        .unwrap()
        .1;
    version.reverse();
    println!(
        "Version: {:?}  Date: {:?}",
        version,
        u32::from_le_bytes(
            session
                .read_bytes_until(&ReadUntil::NBytes(4))
                .await
                .unwrap()
                .1
                .try_into()
                .unwrap()
        )
    );
    let mut processor = session
        .read_bytes_until(&ReadUntil::NBytes(4))
        .await
        .unwrap()
        .1;
    processor.reverse();
    println!("Processor: {}", String::from_utf8(processor).unwrap());
    session
        .exp_bytes(vec![0u8, 0, 0, 0, 0, 0, 0, 0])
        .await
        .expect("Receive GetVersion");
    session.send("&\0\0\0\0\0\0\0").await.unwrap();
    session
        .exp_bytes(vec![b'&', 0, 0, 0, 0o10, 4])
        .await
        .unwrap();
    let mut image_type = session
        .read_bytes_until(&ReadUntil::NBytes(4))
        .await
        .unwrap()
        .1;
    image_type.reverse();
    println!("Image Type: {:?}", image_type);
    session
        .send_bytes(&[b'\'', 0, 0, 0, 1, 0, 0, 0, 0])
        .await
        .unwrap();
    session
        .exp_bytes(vec![b'\'', 0, 0, 0, 0o10, 0])
        .await
        .unwrap();
    let mut cur = 0;
    let mut ctr = 1;
    while cur < btim.1 {
        session
            .send_bytes(&[b'*', ctr, 0, 0, 4, 0, 0, 0])
            .await
            .unwrap();
        session
            .send_bytes(&(btim.1 as u32 - cur as u32).to_le_bytes())
            .await
            .unwrap();
        session
            .exp_bytes(vec![b'*', ctr, 0, 0, 0o10, 4])
            .await
            .unwrap();
        let mut buf: Vec<u8> = repeat(0u8)
            .take(u32::from_le_bytes(
                session
                    .read_bytes_until(&ReadUntil::NBytes(4))
                    .await
                    .unwrap()
                    .1
                    .try_into()
                    .unwrap(),
            ) as usize)
            .collect();
        ctr += 1;
        session.send_bytes(&[b'"', ctr, 0, 0]).await.unwrap();
        session
            .send_bytes(&(buf.len() as u32).to_le_bytes())
            .await
            .unwrap();
        cur += buf.len() as u64;
        btim.0.read_exact(buf.as_mut_slice()).await.unwrap();
        session.send_bytes(&buf).await.unwrap();
        session
            .exp_bytes(vec![b'"', ctr, 0, 0, 0o10, 0])
            .await
            .unwrap();
        ctr += 1;
    }
    session.send_bytes(&[0, 0, 0, 0, 0, 0, 0, 0]).await.unwrap();
    session.exp_bytes(vec![0, 0, 0, 0, 0o10, 0]).await.unwrap();
    session.send_bytes(&[0, 0o323, 2, b'+']).await.unwrap();
    session.exp_bytes(vec![0, 0o323, 2, b'+']).await.unwrap();
    session.send("+\0\0\0\0\0\0\0").await.unwrap();
    session.exp_bytes(vec![b'+', 0, 0, 1, 0, 0]).await.unwrap();
    // skipping get version.
    session.send("&\0\0\0\0\0\0\0").await.unwrap();
    session
        .exp_bytes(vec![b'&', 0, 0, 0, 0o10, 4])
        .await
        .unwrap();

    let mut image_type = session
        .read_bytes_until(&ReadUntil::NBytes(4))
        .await
        .unwrap()
        .1;
    image_type.reverse();
    println!("Image Type: {:?}", image_type);

    session
        .send_bytes(&[b'\'', 0, 0, 0, 1, 0, 0, 0, 0])
        .await
        .unwrap();
    session
        .exp_bytes(vec![b'\'', 0, 0, 0, 0o10, 0])
        .await
        .unwrap();
    let mut cur = 0;
    let mut ctr = 1;
    while cur < image_h[0].1 {
        session
            .send_bytes(&[b'*', ctr, 0, 0, 4, 0, 0, 0])
            .await
            .unwrap();
        session
            .send_bytes(&(image_h[0].1 as u32 - cur as u32).to_le_bytes())
            .await
            .unwrap();
        session
            .exp_bytes(vec![b'*', ctr, 0, 0, 0o10, 4])
            .await
            .unwrap();
        let mut buf: Vec<u8> = repeat(0u8)
            .take(u32::from_le_bytes(
                session
                    .read_bytes_until(&ReadUntil::NBytes(4))
                    .await
                    .unwrap()
                    .1
                    .try_into()
                    .unwrap(),
            ) as usize)
            .collect();
        ctr += 1;
        session.send_bytes(&[b'"', ctr, 0, 0]).await.unwrap();
        session
            .send_bytes(&(buf.len() as u32).to_le_bytes())
            .await
            .unwrap();
        cur += buf.len() as u64;
        image_h[0].0.read_exact(buf.as_mut_slice()).await.unwrap();
        session.send_bytes(&buf).await.unwrap();
        session
            .exp_bytes(vec![b'"', ctr, 0, 0, 0o10, 0])
            .await
            .unwrap();
        ctr += 1;
    }
    session.send_bytes(&[0, 0, 0, 0, 0, 0, 0, 0]).await.unwrap();
    session.exp_bytes(vec![0, 0, 0, 0, 0o10, 0]).await.unwrap();
    session.send_bytes(&[0, 0o323, 2, b'+']).await.unwrap();
    session.exp_bytes(vec![0, 0o323, 2, b'+']).await.unwrap();
    session.send("+\0\0\0\0\0\0\0").await.unwrap();
    session.exp_bytes(vec![b'+', 0, 0, 1, 0, 0]).await.unwrap();
    // skipping get version.
    session.send("&\0\0\0\0\0\0\0").await.unwrap();
    session
        .exp_bytes(vec![b'&', 0, 0, 0, 0o10, 4])
        .await
        .unwrap();

    let mut image_type = session
        .read_bytes_until(&ReadUntil::NBytes(4))
        .await
        .unwrap()
        .1;
    image_type.reverse();
    println!("Image Type: {:?}", image_type);

    session
        .send_bytes(&[b'\'', 0, 0, 0, 1, 0, 0, 0, 0])
        .await
        .unwrap();
    session
        .exp_bytes(vec![b'\'', 0, 0, 0, 0o10, 0])
        .await
        .unwrap();
    let mut cur = 0;
    let mut ctr = 1;
    while cur < image_h[1].1 {
        session
            .send_bytes(&[b'*', ctr, 0, 0, 4, 0, 0, 0])
            .await
            .unwrap();
        session
            .send_bytes(&(image_h[1].1 as u32 - cur as u32).to_le_bytes())
            .await
            .unwrap();
        session
            .exp_bytes(vec![b'*', ctr, 0, 0, 0o10, 4])
            .await
            .unwrap();
        let mut buf: Vec<u8> = repeat(0u8)
            .take(u32::from_le_bytes(
                session
                    .read_bytes_until(&ReadUntil::NBytes(4))
                    .await
                    .unwrap()
                    .1
                    .try_into()
                    .unwrap(),
            ) as usize)
            .collect();
        ctr += 1;
        session.send_bytes(&[b'"', ctr, 0, 0]).await.unwrap();
        session
            .send_bytes(&(buf.len() as u32).to_le_bytes())
            .await
            .unwrap();
        cur += buf.len() as u64;
        image_h[1].0.read_exact(buf.as_mut_slice()).await.unwrap();
        session.send_bytes(&buf).await.unwrap();
        session
            .exp_bytes(vec![b'"', ctr, 0, 0, 0o10, 0])
            .await
            .unwrap();
        ctr += 1;
    }
    session.send_bytes(&[0, 0, 0, 0, 0, 0, 0, 0]).await.unwrap();
    session.exp_bytes(vec![0, 0, 0, 0, 0o10, 0]).await.unwrap();
    session.send_bytes(&[0, 0o323, 2, b'+']).await.unwrap();
    session.exp_bytes(vec![0, 0o323, 2, b'+']).await.unwrap();
    Ok(())
}
