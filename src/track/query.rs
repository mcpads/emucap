use rusqlite::Connection;

#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: String,
    pub rom_sha1: String,
    pub goal: Option<String>,
    pub status: String,
    pub repro_status: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RunFilter {
    pub rom_sha1: Option<String>,
    pub goal: Option<String>,
    pub status: Option<String>,
}

/// 필터에 맞는 run 행을 시작시각 내림차순(최근 우선)으로 반환한다.
pub fn query_runs(conn: &Connection, f: &RunFilter) -> Result<Vec<RunRow>, rusqlite::Error> {
    let mut sql = String::from("SELECT id,rom_sha1,goal,status,repro_status FROM run WHERE 1=1");
    let mut args: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
    if let Some(v) = &f.rom_sha1 {
        sql.push_str(" AND rom_sha1 = :rom");
        args.push((":rom", v));
    }
    if let Some(v) = &f.goal {
        sql.push_str(" AND goal = :goal");
        args.push((":goal", v));
    }
    if let Some(v) = &f.status {
        sql.push_str(" AND status = :status");
        args.push((":status", v));
    }
    sql.push_str(" ORDER BY started_at DESC, id DESC");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(args.as_slice(), |r| {
        Ok(RunRow {
            id: r.get(0)?,
            rom_sha1: r.get(1)?,
            goal: r.get(2)?,
            status: r.get(3)?,
            repro_status: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}
