use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KernelImageFormat {
    Raw,
    Elf,
    ImageBz2,
    ImageGz,
    ImageZstd,
}

pub(crate) fn detect_kernel_image_format(kernel: &Path) -> Result<KernelImageFormat> {
    // Read enough bytes to also detect embedded compressed payloads in wrapped
    // kernel images (e.g. x86_64 bzImage that starts with an MZ header).
    const MAX_PROBE_BYTES: usize = 2 * 1024 * 1024;

    let mut file = File::open(kernel)?;
    let mut buf = vec![0u8; MAX_PROBE_BYTES];
    let n = file.read(&mut buf)?;
    buf.truncate(n);

    if starts_with_magic(&buf, &[0x7F, b'E', b'L', b'F']) {
        return Ok(KernelImageFormat::Elf);
    }

    if starts_with_magic(&buf, &[0x28, 0xB5, 0x2F, 0xFD]) {
        return Ok(KernelImageFormat::ImageZstd);
    }

    if starts_with_magic(&buf, &[0x1F, 0x8B]) {
        return Ok(KernelImageFormat::ImageGz);
    }

    if starts_with_magic(&buf, b"BZh") {
        return Ok(KernelImageFormat::ImageBz2);
    }

    // PE/COFF-wrapped kernels often carry a compressed payload later in the
    // image. Detect the embedded stream and choose the corresponding image
    // format.
    if starts_with_magic(&buf, b"MZ") {
        if contains_magic(&buf, &[0x28, 0xB5, 0x2F, 0xFD]) {
            return Ok(KernelImageFormat::ImageZstd);
        }

        if contains_magic(&buf, &[0x1F, 0x8B]) {
            return Ok(KernelImageFormat::ImageGz);
        }

        if contains_magic(&buf, b"BZh") {
            return Ok(KernelImageFormat::ImageBz2);
        }
    }

    Ok(KernelImageFormat::Raw)
}

fn starts_with_magic(buf: &[u8], magic: &[u8]) -> bool {
    buf.len() >= magic.len() && &buf[..magic.len()] == magic
}

fn contains_magic(buf: &[u8], magic: &[u8]) -> bool {
    !magic.is_empty() && buf.windows(magic.len()).any(|window| window == magic)
}
