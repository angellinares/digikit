fn main() {
    let t = std::time::Instant::now();
    let p = sharc_translate::program().expect("parse");
    let mut fns = 0;
    for it in p.items.values() {
        if let sharc_translate::rs::ast::Item::Fn(_) = it {
            fns += 1;
        }
    }
    println!(
        "items {} fns {} skipped {} in {:?}",
        p.items.len(),
        fns,
        p.skipped.len(),
        t.elapsed()
    );
    let mut sk = p.skipped.clone();
    sk.sort();
    for (n, e) in sk {
        println!("  skipped {n}: {e}");
    }
}
