use std::io::Read as _;
fn u32(p: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(p[o..o + 4].try_into().unwrap())
}
fn main() {
    let base = std::env::args().nth(1).unwrap();
    let cdb = std::fs::read(format!("{base}/iPod_Control/iTunes/iTunesCDB")).unwrap();
    let hl = u32(&cdb, 4) as usize;
    let mut dec = flate2::read::ZlibDecoder::new(&cdb[hl..]);
    let mut payload = Vec::new();
    dec.read_to_end(&mut payload).unwrap();
    let n = u32(&cdb, 0x14) as usize;
    let mut off = 0;
    for _ in 0..n {
        let hdr = u32(&payload, off + 4) as usize;
        let total = u32(&payload, off + 8) as usize;
        let kind = u32(&payload, off + 12);
        if kind == 4 {
            let list = off + hdr;
            let lh = u32(&payload, list + 4) as usize;
            let count = u32(&payload, list + 8);
            println!("CDB mhla count={count}");
            let mut rec = list + lh;
            for i in 0..count.min(2) {
                let rh = u32(&payload, rec + 4) as usize;
                let rt = u32(&payload, rec + 8) as usize;
                let rc = u32(&payload, rec + 12);
                println!("  mhia#{i} hdr={rh} total={rt} children={rc}");
                let mut c = rec + rh;
                for _ in 0..rc.min(2) {
                    let ch = u32(&payload, c + 4) as usize;
                    let ct = u32(&payload, c + 8) as usize;
                    println!(
                        "    child hdr={ch} total={ct} type={}",
                        u32(&payload, c + 12)
                    );
                    println!("    {:02x?}", &payload[c..c + ct.min(80)]);
                    c += ct;
                }
                rec += rt;
            }
        }
        off += total;
    }
}
