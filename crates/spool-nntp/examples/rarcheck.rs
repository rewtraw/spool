fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let data: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    std::fs::write(dir.join("expect.bin"), &data).unwrap();
    for (i, v) in spool_nntp::rar::testing::rar5_volumes("five.bin", &data, 120_000).iter().enumerate() {
        std::fs::write(dir.join(format!("five.part{}.rar", i + 1)), v).unwrap();
    }
    for (i, v) in spool_nntp::rar::testing::rar4_volumes("four.bin", &data, 120_000).iter().enumerate() {
        std::fs::write(dir.join(if i == 0 { "four.rar".to_string() } else { format!("four.r{:02}", i - 1) }), v).unwrap();
    }
}
