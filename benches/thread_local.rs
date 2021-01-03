extern crate criterion;
extern crate thread_local;

use criterion::{black_box, BatchSize};

use thread_local::{CachedThreadLocal, ThreadLocal};

fn main() {
    let mut c = criterion::Criterion::default().configure_from_args();

    c.bench_function("get", |b| {
        let local = ThreadLocal::new();
        local.get_or(|| Box::new(0));
        b.iter(|| {
            black_box(local.get());
        });
    });

    c.bench_function("get_cached", |b| {
        let local = CachedThreadLocal::new();
        local.get_or(|| Box::new(0));
        b.iter(|| {
            black_box(local.get());
        });
    });

    c.bench_function("get_cached_second_thread", |b| {
        let local = CachedThreadLocal::new();
        local.get();
        let local = std::thread::spawn(move || {
            local.get_or(|| Box::new(0));
            local
        })
        .join()
        .unwrap();

        local.get_or(|| Box::new(0));

        b.iter(|| {
            black_box(local.get());
        });
    });

    c.bench_function("insert", |b| {
        b.iter_batched_ref(
            ThreadLocal::new,
            |local| {
                black_box(local.get_or(|| 0));
            },
            BatchSize::SmallInput,
        )
    });

    c.bench_function("insert_cached", |b| {
        b.iter_batched_ref(
            CachedThreadLocal::new,
            |local| {
                black_box(local.get_or(|| 0));
            },
            BatchSize::SmallInput,
        )
    });
}
