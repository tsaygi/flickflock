//! Benchmarks for the two latencies users feel most: generating a thumbnail
//! and getting the next image ready for display.
//!
//!     cargo bench -p ff-core

use criterion::{criterion_group, criterion_main, Criterion};
use ff_core::cache::ThumbDiskCache;
use ff_core::decode::Decoders;
use ff_core::folder::{FolderIndex, SortSpec};
use ff_core::jobs::CancelToken;
use ff_core::loader::{load, Level, LoadJob};
use ff_core::thumbs::make_thumbnail;
use image::{DynamicImage, Rgb, RgbImage};
use std::hint::black_box;
use std::path::{Path, PathBuf};

fn photo(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
    let path = dir.join(name);
    let img = RgbImage::from_fn(w, h, |x, y| {
        // Smooth gradients plus some texture, roughly like a photograph.
        let n = ((x * 7 + y * 13) % 31) as u8;
        Rgb([
            (x * 255 / w) as u8 ^ n,
            (y * 255 / h) as u8,
            128u8.wrapping_add(n),
        ])
    });
    DynamicImage::ImageRgb8(img)
        .save(&path)
        .expect("write fixture");
    path
}

fn benches(c: &mut Criterion) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let decoders = Decoders::builtin();
    let small = photo(tmp.path(), "12mp.jpg", 4000, 3000);
    let large = photo(tmp.path(), "24mp.jpg", 6000, 4000);
    let png = photo(tmp.path(), "2mp.png", 1920, 1080);
    let job = LoadJob {
        preview_dim: 2560,
        tile_size: 8192,
        animate: false,
    };

    let mut group = c.benchmark_group("thumbnail");
    group.sample_size(20);
    group.bench_function("12 MP JPEG, cold", |b| {
        b.iter(|| black_box(make_thumbnail(&small, &decoders, None)))
    });
    let cache = ThumbDiskCache::new(tmp.path().join("cache"));
    make_thumbnail(&small, &decoders, Some(&cache));
    group.bench_function("12 MP JPEG, disk cache hit", |b| {
        b.iter(|| black_box(make_thumbnail(&small, &decoders, Some(&cache))))
    });
    group.finish();

    // "Next image latency" without prefetch: decode + downscale + mip chain.
    let mut group = c.benchmark_group("next image");
    group.sample_size(10);
    for (name, path) in [
        ("2 MP PNG", &png),
        ("12 MP JPEG", &small),
        ("24 MP JPEG", &large),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                black_box(load(
                    path,
                    Level::Preview,
                    &job,
                    &decoders,
                    &CancelToken::default(),
                ))
            })
        });
    }
    group.finish();

    // Folder indexing: 2,000 files.
    let many = tmp.path().join("many");
    std::fs::create_dir(&many).expect("mkdir");
    for i in 0..2000 {
        std::fs::write(many.join(format!("IMG_{i}.jpg")), b"x").expect("write");
    }
    c.bench_function("index 2000 files", |b| {
        b.iter(|| black_box(FolderIndex::scan(&many, &SortSpec::default())))
    });
}

criterion_group!(perf, benches);
criterion_main!(perf);
