#[macro_use]
extern crate tracing;

use std::{env, path::Path};

use futures::future::FutureExt;
use structopt::StructOpt;

use mayastor::{
    bdev::util::uring,
    core::{
        device_monitor,
        runtime,
        MayastorCliArgs,
        MayastorEnvironment,
        Mthread,
        Reactors,
    },
    grpc,
    logger,
    persistent_store::PersistentStore,
    subsys,
    subsys::Registration,
};
use version_info::fmt_package_info;

const PAGES_NEEDED: u32 = 1024;

mayastor::CPS_INIT!();
fn start_tokio_runtime(args: &MayastorCliArgs) {
    let grpc_address = grpc::endpoint(args.grpc_endpoint.clone());
    let rpc_address = args.rpc_address.clone();
    // In case we do not have the node-name provided we would set the node name
    // as the hostname(env always present), because the csi-controller adds
    // the hostname in allowed nodes in the topology and in case there is
    // mismatch, for ex, in case of EKS clusters where hostname and
    // node name differ volume wont be created, so we set it to hostname.
    let node_name = args.node_name.clone().unwrap_or_else(|| {
        env::var("HOSTNAME").unwrap_or_else(|_| "mayastor-node".into())
    });

    let endpoint = args.mbus_endpoint.clone();
    let persistent_store_endpoint = args.persistent_store_endpoint.clone();

    Mthread::spawn_unaffinitized(move || {
        runtime::block_on(async move {
            let mut futures = Vec::new();
            if let Some(endpoint) = endpoint {
                debug!("mayastor mbus subsystem init");
                mbus_api::message_bus_init_tokio(endpoint);
                Registration::init(&node_name, &grpc_address.to_string());
                futures.push(subsys::Registration::run().boxed());
            }

            PersistentStore::init(persistent_store_endpoint).await;
            runtime::spawn(device_monitor());

            futures.push(
                grpc::MayastorGrpcServer::run(grpc_address, rpc_address)
                    .boxed(),
            );

            futures::future::try_join_all(futures)
                .await
                .expect_err("runtime exited in the abnormal state");
        });
    });
}

fn hugepage_check() {
    let hugepage_path = Path::new("/sys/kernel/mm/hugepages/hugepages-2048kB");
    let nr_pages: u32 = sysfs::parse_value(hugepage_path, "nr_hugepages")
        .expect("failed to read the number of pages");
    let free_pages: u32 = sysfs::parse_value(hugepage_path, "free_hugepages")
        .expect("failed to read the number of free pages");
    if nr_pages < PAGES_NEEDED {
        error!(?PAGES_NEEDED, ?nr_pages, "insufficient pages available");
        if !cfg!(debug_assertions) {
            std::process::exit(1)
        }
    }

    if free_pages < PAGES_NEEDED {
        error!(
            ?PAGES_NEEDED,
            ?nr_pages,
            "insufficient free pages available"
        );
        if !cfg!(debug_assertions) {
            std::process::exit(1)
        }
    }

    info!("free_pages: {} nr_pages: {}", free_pages, nr_pages);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = MayastorCliArgs::from_args();

    // setup our logger first if -L is passed, raise the log level
    // automatically. trace maps to debug at FFI level. If RUST_LOG is
    // passed, we will use it regardless.

    if !args.log_components.is_empty() {
        logger::init("TRACE");
    } else {
        logger::init("INFO");
    }

    info!("{}", fmt_package_info!());

    hugepage_check();

    let nvme_core_path = Path::new("/sys/module/nvme_core/parameters");
    let nvme_mp: String =
        match sysfs::parse_value::<String>(nvme_core_path, "multipath") {
            Ok(s) => match s.as_str() {
                "Y" => "yes".to_string(),
                "N" => "disabled".to_string(),
                u => format!("unknown value {}", u),
            },
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    if nvme_core_path.exists() {
                        "not built".to_string()
                    } else {
                        "nvme not loaded".to_string()
                    }
                } else {
                    format!("unknown error: {}", e)
                }
            }
        };

    info!(
        "kernel io_uring support: {}",
        if uring::kernel_support() { "yes" } else { "no" }
    );
    info!("kernel nvme initiator multipath support: {}", nvme_mp);

    let ms = MayastorEnvironment::new(args.clone()).init();
    start_tokio_runtime(&args);

    Reactors::current().running();
    Reactors::current().poll_reactor();

    ms.fini();
    Ok(())
}
