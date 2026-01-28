// Responsibility:
// - Ensure required Windows resources exist (icon) for tauri-build.
// - Delegate to tauri-build afterwards.

use std::{fs, path::PathBuf};

const ICON_RELATIVE_PATH: &str = "icons/icon.ico";

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const PNG_CHUNK_TYPE_IHDR: &[u8; 4] = b"IHDR";
const PNG_CHUNK_TYPE_IDAT: &[u8; 4] = b"IDAT";
const PNG_CHUNK_TYPE_IEND: &[u8; 4] = b"IEND";

const PNG_WIDTH_PIXELS: u32 = 1;
const PNG_HEIGHT_PIXELS: u32 = 1;
const PNG_BIT_DEPTH: u8 = 8;
const PNG_COLOR_TYPE_RGBA: u8 = 6;
const PNG_COMPRESSION_METHOD_DEFLATE: u8 = 0;
const PNG_FILTER_METHOD_ADAPTIVE: u8 = 0;
const PNG_INTERLACE_METHOD_NONE: u8 = 0;

const ZLIB_HEADER_CM8_CINFO7: u8 = 0x78;
const ZLIB_HEADER_FLEVEL0_FDICT0_FCHECK: u8 = 0x01;

fn crc32_ieee(bytes: &[u8]) -> u32 {
  let mut crc: u32 = 0xFFFF_FFFF;
  for &byte in bytes {
    let mut x = crc ^ (byte as u32);
    for _ in 0..8 {
      let mask = if (x & 1) == 1 { 0xEDB8_8320 } else { 0 };
      x = (x >> 1) ^ mask;
    }
    crc = x;
  }
  !crc
}

fn adler32(bytes: &[u8]) -> u32 {
  const MOD_ADLER: u32 = 65521;
  let mut a: u32 = 1;
  let mut b: u32 = 0;
  for &byte in bytes {
    a = (a + byte as u32) % MOD_ADLER;
    b = (b + a) % MOD_ADLER;
  }
  (b << 16) | a
}

fn write_png_chunk(destination: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
  destination.extend_from_slice(&(data.len() as u32).to_be_bytes());
  destination.extend_from_slice(chunk_type);
  destination.extend_from_slice(data);
  let mut crc_input: Vec<u8> = Vec::with_capacity(4 + data.len());
  crc_input.extend_from_slice(chunk_type);
  crc_input.extend_from_slice(data);
  destination.extend_from_slice(&crc32_ieee(&crc_input).to_be_bytes());
}

fn generate_png_1x1_transparent() -> Vec<u8> {
  // Uncompressed scanline: filter(0) + RGBA(0,0,0,0)
  let raw_image_data: [u8; 5] = [0, 0, 0, 0, 0];

  // Build zlib stream using a single stored (uncompressed) DEFLATE block.
  // - zlib header
  // - DEFLATE block header: BFINAL=1, BTYPE=00 => 0x01
  // - LEN/NLEN (little-endian)
  // - raw data
  // - Adler32 checksum
  let len: u16 = raw_image_data.len() as u16;
  let nlen: u16 = !len;
  let mut zlib_stream: Vec<u8> = Vec::new();
  zlib_stream.push(ZLIB_HEADER_CM8_CINFO7);
  zlib_stream.push(ZLIB_HEADER_FLEVEL0_FDICT0_FCHECK);
  zlib_stream.push(0x01);
  zlib_stream.extend_from_slice(&len.to_le_bytes());
  zlib_stream.extend_from_slice(&nlen.to_le_bytes());
  zlib_stream.extend_from_slice(&raw_image_data);
  zlib_stream.extend_from_slice(&adler32(&raw_image_data).to_be_bytes());

  let mut png_bytes: Vec<u8> = Vec::new();
  png_bytes.extend_from_slice(PNG_SIGNATURE);

  let mut ihdr_data: Vec<u8> = Vec::with_capacity(13);
  ihdr_data.extend_from_slice(&PNG_WIDTH_PIXELS.to_be_bytes());
  ihdr_data.extend_from_slice(&PNG_HEIGHT_PIXELS.to_be_bytes());
  ihdr_data.push(PNG_BIT_DEPTH);
  ihdr_data.push(PNG_COLOR_TYPE_RGBA);
  ihdr_data.push(PNG_COMPRESSION_METHOD_DEFLATE);
  ihdr_data.push(PNG_FILTER_METHOD_ADAPTIVE);
  ihdr_data.push(PNG_INTERLACE_METHOD_NONE);
  write_png_chunk(&mut png_bytes, PNG_CHUNK_TYPE_IHDR, &ihdr_data);

  write_png_chunk(&mut png_bytes, PNG_CHUNK_TYPE_IDAT, &zlib_stream);
  write_png_chunk(&mut png_bytes, PNG_CHUNK_TYPE_IEND, &[]);

  png_bytes
}

fn icon_file_path() -> PathBuf {
  let manifest_directory_path = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
  manifest_directory_path.join(ICON_RELATIVE_PATH)
}

fn generate_minimal_ico_with_png_bytes() -> Vec<u8> {
  // ICO header (ICONDIR)
  // - reserved: 0
  // - type: 1 (icon)
  // - count: 1
  let mut bytes: Vec<u8> = Vec::new();
  bytes.extend_from_slice(&0u16.to_le_bytes());
  bytes.extend_from_slice(&1u16.to_le_bytes());
  bytes.extend_from_slice(&1u16.to_le_bytes());

  // Directory entry (ICONDIRENTRY)
  // width/height: 1
  // color count/reserved: 0
  // planes: 1
  // bitcount: 32 (arbitrary; PNG data is authoritative)
  let png_bytes = generate_png_1x1_transparent();
  // bytes in resource: PNG length
  // image offset: 6 + 16 = 22
  let image_offset: u32 = 6 + 16;
  bytes.push(1);
  bytes.push(1);
  bytes.push(0);
  bytes.push(0);
  bytes.extend_from_slice(&1u16.to_le_bytes());
  bytes.extend_from_slice(&32u16.to_le_bytes());
  bytes.extend_from_slice(&(png_bytes.len() as u32).to_le_bytes());
  bytes.extend_from_slice(&image_offset.to_le_bytes());

  bytes.extend_from_slice(&png_bytes);
  bytes
}

fn ensure_icon_exists() {
  let destination_path = icon_file_path();
  let expected_bytes = generate_minimal_ico_with_png_bytes();

  if let Ok(existing_bytes) = fs::read(&destination_path) {
    if existing_bytes == expected_bytes {
      return;
    }
  }

  if let Some(parent) = destination_path.parent() {
    if fs::create_dir_all(parent).is_err() {
      return;
    }
  }

  let _ = fs::write(&destination_path, expected_bytes);
}

fn main() {
  ensure_icon_exists();
  tauri_build::build();
}

