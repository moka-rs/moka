// https://github.com/moka-rs/moka/issues/31

use moka::future::Cache;
use std::rc::Rc;

#[tokio::main]
async fn main() {
    let cache: Cache<_, String> = Cache::new(100);

    // Rc is !Send.
    let data = Rc::new("zero".to_string());
    let data1 = Rc::clone(&data);

    cache
        .get_or_try_insert_with(0, async move {
            // A data race may occur. 
            // The async block can be executed by a different thread
            // but Rc's internal reference counters are not thread safe.
            Ok(data1.to_string())
        })
        .await;

    println!("{:?}", data);
}
