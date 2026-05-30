use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use engine_core::PowTask;
use enginelib::task::Task;

fn bench_pow_s(c: &mut Criterion) {
    let mut group = c.benchmark_group("pow/s");

    for zeros in [3_u8, 4_u8] {
        group.bench_with_input(BenchmarkId::from_parameter(zeros), &zeros, |b, &zeros| {
            b.iter(|| {
                let mut task = PowTask {
                    seed: 42,
                    zeros,
                    nonce: 0,
                };
                task.run_cpu();
                criterion::black_box(task.nonce);
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_pow_s);
criterion_main!(benches);
