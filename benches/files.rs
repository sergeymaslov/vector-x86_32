use bytes::Bytes;
use criterion::{criterion_group, Benchmark, Criterion, Throughput};
use futures01::{sink::Sink, stream::Stream, Future};
use std::convert::TryInto;
use std::path::PathBuf;
use tempfile::tempdir;
use tokio01::codec::{BytesCodec, FramedWrite};
use tokio01::fs::OpenOptions;
use vector::test_util::random_lines;
use vector::{
    runtime, sinks, sources,
    topology::{self, config},
};

fn benchmark_files_without_partitions(c: &mut Criterion) {
    let num_lines: usize = 100_000;
    let line_size: usize = 100;

    let bench = Benchmark::new("files_without_partitions", move |b| {
        let temp = tempdir().unwrap();
        let directory = temp.path().to_path_buf();

        b.iter_with_setup(
            || {
                let directory_str = directory.to_str().unwrap();

                let mut data_dir = directory_str.to_owned();
                data_dir.push_str("/data");
                let data_dir = PathBuf::from(data_dir);
                std::fs::remove_dir_all(&data_dir).unwrap_or(());
                std::fs::create_dir(&data_dir).unwrap_or(());
                //if it doesn't exist -- that's ok

                let mut input = directory_str.to_owned();
                input.push_str("/test.in");

                let input = PathBuf::from(input);

                let mut output = directory_str.to_owned();
                output.push_str("/test.out");

                let mut source = sources::file::FileConfig::default();
                source.include.push(input.clone());
                source.data_dir = Some(data_dir);

                let mut config = config::Config::empty();
                config.add_source("in", source);
                config.add_sink(
                    "out",
                    &["in"],
                    sinks::file::FileSinkConfig {
                        path: output.try_into().unwrap(),
                        idle_timeout_secs: None,
                        encoding: sinks::file::Encoding::Text.into(),
                    },
                );

                let mut rt = runtime::Runtime::new().unwrap();
                let (topology, _crash) = rt.block_on_std(topology::start(config, false)).unwrap();

                let mut options = OpenOptions::new();
                options.create(true).write(true);

                let input = rt.block_on(options.open(input)).unwrap();
                let input =
                    FramedWrite::new(input, BytesCodec::new()).sink_map_err(|e| panic!("{:?}", e));

                (rt, topology, input)
            },
            |(mut rt, topology, input)| {
                let lines = random_lines(line_size).take(num_lines).map(|mut line| {
                    line.push('\n');
                    Bytes::from(line)
                });

                let lines = futures01::stream::iter_ok::<_, ()>(lines);

                let pump = lines.forward(input);
                let (_, _) = rt.block_on(pump).unwrap();

                rt.block_on(topology.stop()).unwrap();
                rt.shutdown_now().wait().unwrap();
            },
        )
    })
    .sample_size(10)
    .noise_threshold(0.05)
    .throughput(Throughput::Bytes((num_lines * line_size) as u64));

    c.bench("files", bench);
}

criterion_group!(files, benchmark_files_without_partitions);
