//! overview_cache —— 概览冻结区按天缓存（hardening-v4 §4.3 H3）。
//!
//! 冻结边界 `frozen_upto = today − 2 天`：早于“昨天”的日期在正常运行中不会再被写入；
//! “昨天 + 今天”始终作为实时段查询（collector 跨日最后一个 flush 周期及 flush 重试
//! 仍可能写入“昨天”）。
//!
//! 合同：
//! - 合并结果与单次全量 `reader::overview(from, to)` 逐项相等（性质测试保证）；
//! - 键 `(from, frozen_upto, generation)`，最多 [`MAX_ENTRIES`] 条，LRU 淘汰；
//! - 条目超过 [`ENTRY_TTL`] 强制重算（安全网）；
//! - 只缓存成功结果；兜底内存连接上的结果不缓存（调用方在 `get_overview` 中判定）；
//! - `generation` 递增等价清空：ro 连接从兜底切到真实库、WhatPulse 导入完成、schema 过新状态变化。
//!   任何未来新增的写 `input_daily` 历史日期的功能（删除/合并设备、历史编辑）必须同步递增。
//!
//! 锁序：缓存锁独立于 `ro_conn` 锁；计算期间持有 ro 锁，只在读写缓存时短暂持有缓存锁。

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::{Days, NaiveDate};
use clrecoder_store::reader::{self, OverviewData, OverviewParts};
use clrecoder_store::Result;
use rusqlite::Connection;

/// 最多缓存条目数（本周、本月、一年、全部等不同起点）。
pub(crate) const MAX_ENTRIES: usize = 4;
/// 条目强制重算间隔（覆盖“flush 失败重试持续超过一天”等极端情形）。
pub(crate) const ENTRY_TTL: Duration = Duration::from_secs(10 * 60);

struct Entry {
    from: String,
    frozen_upto: String,
    generation: u64,
    created: Instant,
    last_used: u64,
    parts: Arc<OverviewParts>,
}

/// 冻结段缓存（`AppState` 以 `Arc<Mutex<_>>` 持有）。
#[derive(Default)]
pub struct OverviewCache {
    entries: Vec<Entry>,
    generation: u64,
    tick: u64,
}

impl OverviewCache {
    /// 当前代际。
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// 代际 +1（等价清空），同时丢弃全部条目释放内存。
    pub(crate) fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.entries.clear();
    }

    /// 查找未过期的命中条目。
    fn get(&mut self, from: &str, frozen_upto: &str, now: Instant) -> Option<Arc<OverviewParts>> {
        let generation = self.generation;
        self.tick += 1;
        let tick = self.tick;
        self.entries.retain(|e| now.saturating_duration_since(e.created) < ENTRY_TTL);
        let e = self
            .entries
            .iter_mut()
            .find(|e| e.from == from && e.frozen_upto == frozen_upto && e.generation == generation)?;
        e.last_used = tick;
        Some(Arc::clone(&e.parts))
    }

    /// 写入条目；`generation` 为计算开始前读到的代际——计算期间发生失效则丢弃本次结果。
    fn put(&mut self, from: &str, frozen_upto: &str, generation: u64, parts: Arc<OverviewParts>, now: Instant) {
        if generation != self.generation {
            return;
        }
        self.tick += 1;
        self.entries.retain(|e| !(e.from == from && e.frozen_upto == frozen_upto));
        if self.entries.len() >= MAX_ENTRIES {
            if let Some(i) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(i, _)| i)
            {
                self.entries.swap_remove(i);
            }
        }
        self.entries.push(Entry {
            from: from.to_string(),
            frozen_upto: frozen_upto.to_string(),
            generation,
            created: now,
            last_used: self.tick,
            parts,
        });
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

fn lock(cache: &Mutex<OverviewCache>) -> std::sync::MutexGuard<'_, OverviewCache> {
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 冻结边界：`today − 2 天`（`YYYY-MM-DD`）。
pub(crate) fn frozen_upto(today: NaiveDate) -> NaiveDate {
    today.checked_sub_days(Days::new(2)).unwrap_or(NaiveDate::MIN)
}

/// 带冻结区缓存的概览查询（纯函数：`today`/`now` 由调用方注入，不读系统时钟）。
///
/// 1. `to <= frozen_upto` 或 `from > frozen_upto`：直接走原 `reader::overview`；
/// 2. 否则冻结段 `[from, frozen_upto]` 查缓存（未命中则 `overview_parts` 计算并存入），
///    实时段 `[frozen_upto+1, to]` 走原 `reader::overview`，按设备 id 合并 total。
pub(crate) fn cached_overview(
    conn: &Connection,
    cache: &Mutex<OverviewCache>,
    from: &str,
    to: &str,
    today: NaiveDate,
    now: Instant,
) -> Result<OverviewData> {
    let frozen = frozen_upto(today);
    let frozen_s = frozen.format("%Y-%m-%d").to_string();
    if to <= frozen_s.as_str() || from > frozen_s.as_str() || from > to {
        return reader::overview(conn, from, to);
    }
    let live_from = frozen
        .checked_add_days(Days::new(1))
        .unwrap_or(frozen)
        .format("%Y-%m-%d")
        .to_string();

    let (hit, generation) = {
        let mut c = lock(cache);
        (c.get(from, &frozen_s, now), c.generation())
    };
    let parts = match hit {
        Some(p) => p,
        None => {
            // 只缓存成功结果：Err 直接上抛，不写缓存。
            let p = Arc::new(reader::overview_parts(conn, from, &frozen_s)?);
            lock(cache).put(from, &frozen_s, generation, Arc::clone(&p), now);
            p
        }
    };

    let mut live = reader::overview(conn, &live_from, to)?;
    let mut days = parts.days.clone();
    days.append(&mut live.days);
    live.days = days;
    for d in &mut live.devices {
        if let Ok(i) = parts.device_totals.binary_search_by_key(&d.id, |(id, _)| *id) {
            d.total = d.total.saturating_add(parts.device_totals[i].1);
        }
    }
    Ok(live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::overview::OverviewDto;
    use crate::state::testutil::TempFile;
    use clrecoder_core::codes::DeviceKind;
    use clrecoder_core::event::DeviceKey;
    use clrecoder_store::writer::{FlushBatch, Writer};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn fmt(x: NaiveDate) -> String {
        x.format("%Y-%m-%d").to_string()
    }

    fn json(o: OverviewData) -> serde_json::Value {
        serde_json::to_value(OverviewDto::from(o)).unwrap()
    }

    /// 简单可复现伪随机（依赖白名单无 rand）。
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    const TODAY: &str = "2026-10-12";

    /// 3 设备（+1 个无输入设备）× 400 天随机数据（部分日期留空），返回 (文件, 读连接, writer)。
    fn seed_random(tag: &str, seed: u64) -> (TempFile, Connection, Writer, Vec<i64>) {
        let f = TempFile::new(tag, "db");
        let w = Writer::open(f.as_ref()).unwrap();
        let mut ids = Vec::new();
        for (i, kind) in [DeviceKind::Keyboard, DeviceKind::Mouse, DeviceKind::Gamepad, DeviceKind::Mouse]
            .into_iter()
            .enumerate()
        {
            ids.push(
                w.get_or_create_device(&DeviceKey {
                    kind,
                    vid: i as u16,
                    pid: i as u16,
                    name: format!("设备{i}"),
                })
                .unwrap(),
            );
        }
        let mut rng = Lcg(seed);
        let today = d(TODAY);
        let mut input = Vec::new();
        for back in 0..400u64 {
            let day = fmt(today.checked_sub_days(Days::new(back)).unwrap());
            for &id in &ids[..3] {
                if rng.next().is_multiple_of(4) {
                    continue; // 留空日
                }
                for _ in 0..(1 + rng.next() % 3) {
                    let code = (rng.next() % 50) as u16 + 1;
                    input.push((id, day.clone(), code, rng.next() % 500 + 1));
                }
            }
        }
        w.flush(&FlushBatch { input, ..Default::default() }).unwrap();
        let conn = Connection::open(f.as_ref()).unwrap();
        (f, conn, w, ids)
    }

    fn ranges(today: NaiveDate) -> Vec<(String, String)> {
        let back = |n: u64| fmt(today.checked_sub_days(Days::new(n)).unwrap());
        let fwd = |n: u64| fmt(today.checked_add_days(Days::new(n)).unwrap());
        vec![
            (back(6), back(0)),     // 本周
            (back(30), back(0)),    // 本月
            (back(365), back(0)),   // 一年
            ("1970-01-01".into(), back(0)), // 全部
            (back(500), fwd(3)),    // 起点早于数据、终点在未来
            (back(2), back(0)),     // from == frozen_upto
            (back(3), back(1)),     // to == 昨天
            (back(40), back(10)),   // to <= frozen_upto（原路径）
            (back(2), back(2)),     // from == to == frozen_upto（原路径）
            (back(0), back(0)),     // from == to == 今天（纯实时）
            (back(1), back(0)),     // from > frozen_upto（纯实时）
            (back(0), back(5)),     // from > to（原路径，空结果语义不变）
        ]
    }

    /// 性质测试：多组种子 × 多组 (from, to, today)，缓存结果 == 全量 reader::overview；
    /// 第二次（命中）同样相等。
    #[test]
    fn cached_equals_full_property() {
        for seed in [1u64, 7, 42, 2026] {
            let (_f, conn, _w, _) = seed_random(&format!("ovc-prop-{seed}"), seed);
            let cache = Mutex::new(OverviewCache::default());
            let now = Instant::now();
            for today_back in [0u64, 1, 2, 30, 399] {
                let today = d(TODAY).checked_sub_days(Days::new(today_back)).unwrap();
                for (from, to) in ranges(today) {
                    let full = json(reader::overview(&conn, &from, &to).unwrap());
                    for round in 0..2 {
                        let got = json(cached_overview(&conn, &cache, &from, &to, today, now).unwrap());
                        assert_eq!(got, full, "seed={seed} today={today} {from}..{to} round={round}");
                    }
                }
            }
            assert!(lock(&cache).len() <= MAX_ENTRIES);
        }
    }

    /// 失效：冻结段写入新行后，未递增 generation 时返回旧值（证明缓存生效），递增后与全量相等。
    #[test]
    fn generation_invalidates_frozen_rows() {
        let (_f, conn, w, ids) = seed_random("ovc-gen", 3);
        let cache = Mutex::new(OverviewCache::default());
        let today = d(TODAY);
        let (from, to) = ("2026-01-01", TODAY);
        let now = Instant::now();
        let before = json(cached_overview(&conn, &cache, from, to, today, now).unwrap());
        w.flush(&FlushBatch { input: vec![(ids[0], "2026-03-01".into(), 0x1E, 100_000)], ..Default::default() })
            .unwrap();
        let stale = json(cached_overview(&conn, &cache, from, to, today, now).unwrap());
        assert_eq!(stale, before, "未失效时冻结段命中缓存");
        let full = json(reader::overview(&conn, from, to).unwrap());
        assert_ne!(full, before);
        lock(&cache).bump();
        assert_eq!(json(cached_overview(&conn, &cache, from, to, today, now).unwrap()), full);
    }

    /// 10 分钟 TTL（可注入时钟）：未到期命中旧值，到期后重算。
    #[test]
    fn ttl_forces_recompute() {
        let (_f, conn, w, ids) = seed_random("ovc-ttl", 5);
        let cache = Mutex::new(OverviewCache::default());
        let today = d(TODAY);
        let (from, to) = ("2026-01-01", TODAY);
        let t0 = Instant::now();
        let before = json(cached_overview(&conn, &cache, from, to, today, t0).unwrap());
        w.flush(&FlushBatch { input: vec![(ids[1], "2026-02-01".into(), 1, 777)], ..Default::default() })
            .unwrap();
        let t1 = t0 + ENTRY_TTL - Duration::from_secs(1);
        assert_eq!(json(cached_overview(&conn, &cache, from, to, today, t1).unwrap()), before);
        let t2 = t0 + ENTRY_TTL;
        let full = json(reader::overview(&conn, from, to).unwrap());
        assert_eq!(json(cached_overview(&conn, &cache, from, to, today, t2).unwrap()), full);
    }

    /// 只缓存成功结果：首次在未建表连接上失败，不写缓存；随后真实连接得到正确值。
    #[test]
    fn errors_are_not_cached() {
        let (_f, conn, _w, _) = seed_random("ovc-err", 9);
        let cache = Mutex::new(OverviewCache::default());
        let today = d(TODAY);
        let bad = Connection::open_in_memory().unwrap();
        assert!(cached_overview(&bad, &cache, "2026-01-01", TODAY, today, Instant::now()).is_err());
        assert_eq!(lock(&cache).len(), 0);
        let full = json(reader::overview(&conn, "2026-01-01", TODAY).unwrap());
        let got = json(cached_overview(&conn, &cache, "2026-01-01", TODAY, today, Instant::now()).unwrap());
        assert_eq!(got, full);
    }

    /// 冻结命中时，新增设备与昵称修改即时可见（设备列表以实时查询为准）。
    #[test]
    fn device_list_and_nickname_are_live() {
        let (f, conn, w, ids) = seed_random("ovc-dev", 11);
        let cache = Mutex::new(OverviewCache::default());
        let today = d(TODAY);
        let now = Instant::now();
        let _ = cached_overview(&conn, &cache, "2026-01-01", TODAY, today, now).unwrap();
        let new_id = w
            .get_or_create_device(&DeviceKey { kind: DeviceKind::Keyboard, vid: 9, pid: 9, name: "新设备".into() })
            .unwrap();
        let rw = Connection::open(f.as_ref()).unwrap();
        rw.execute("UPDATE devices SET nickname = '昵称X' WHERE id = ?1", [ids[0]]).unwrap();
        let got = cached_overview(&conn, &cache, "2026-01-01", TODAY, today, now).unwrap();
        assert!(got.devices.iter().any(|x| x.id == new_id && x.total == 0));
        assert!(got.devices.iter().any(|x| x.id == ids[0] && x.name == "昵称X"));
        assert_eq!(json(got), json(reader::overview(&conn, "2026-01-01", TODAY).unwrap()));
    }

    /// 计算期间发生失效（generation 变化）时不写入旧代际结果。
    #[test]
    fn put_with_stale_generation_is_dropped() {
        let mut c = OverviewCache::default();
        let g = c.generation();
        c.bump();
        c.put("a", "b", g, Arc::new(OverviewParts::default()), Instant::now());
        assert_eq!(c.len(), 0);
    }

    /// LRU 上限：超过 MAX_ENTRIES 时淘汰最久未用条目。
    #[test]
    fn lru_capacity() {
        let mut c = OverviewCache::default();
        let now = Instant::now();
        for i in 0..6 {
            c.put(&format!("f{i}"), "x", 0, Arc::new(OverviewParts::default()), now);
        }
        assert_eq!(c.len(), MAX_ENTRIES);
        assert!(c.get("f0", "x", now).is_none());
        assert!(c.get("f5", "x", now).is_some());
    }

    /// 基准（手动：`cargo test -- --ignored overview_cache_bench`）：两年 × 5 设备约 16 万行，
    /// 稳态（冻结段命中）单次查询耗时。
    #[test]
    #[ignore]
    fn overview_cache_bench() {
        let f = TempFile::new("ovc-bench", "db");
        let w = Writer::open(f.as_ref()).unwrap();
        let mut ids = Vec::new();
        for i in 0..5u16 {
            let kind = if i < 2 { DeviceKind::Keyboard } else { DeviceKind::Mouse };
            ids.push(w.get_or_create_device(&DeviceKey { kind, vid: i, pid: i, name: format!("d{i}") }).unwrap());
        }
        let today = d(TODAY);
        let mut input = Vec::new();
        for back in 0..730u64 {
            let day = fmt(today.checked_sub_days(Days::new(back)).unwrap());
            for &id in &ids {
                for code in 1..=44u16 {
                    input.push((id, day.clone(), code, 3));
                }
            }
        }
        let rows = input.len();
        w.flush(&FlushBatch { input, ..Default::default() }).unwrap();
        let conn = Connection::open(f.as_ref()).unwrap();
        let cache = Mutex::new(OverviewCache::default());
        for (from, label) in [(fmt(today.checked_sub_days(Days::new(365)).unwrap()), "一年"), ("1970-01-01".to_string(), "全部")] {
            let t = Instant::now();
            let _ = reader::overview(&conn, &from, TODAY).unwrap();
            let full = t.elapsed();
            let _ = cached_overview(&conn, &cache, &from, TODAY, today, Instant::now()).unwrap();
            let n = 200u32;
            let t = Instant::now();
            for _ in 0..n {
                let _ = cached_overview(&conn, &cache, &from, TODAY, today, Instant::now()).unwrap();
            }
            let steady = t.elapsed() / n;
            eprintln!("[bench] rows={rows} 范围={label} 全量={full:?} 缓存稳态={steady:?}");
        }
    }
}
