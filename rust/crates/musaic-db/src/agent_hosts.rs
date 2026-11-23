//! What each paired host reported about itself (spec 005): its name and
//! version as it gave them, the protocol profile it speaks and which
//! extensions it declared or used. Diagnostics only: nothing decides from it.

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    pub client_name: String,
    pub client_version: String,
    pub profile: String,
    pub tasks: bool,
    pub skills: bool,
    pub events: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub observation: Observation,
    pub observed_at: i64,
}

/// Store the latest observation of a pairing. Returns whether what is stored
/// changed (a repeat of the same values only refreshes the time and is not a
/// change). A pairing that does not exist stores nothing.
pub fn upsert(conn: &Connection, pairing_id: &str, o: &Observation, now: i64) -> Result<bool> {
    let known: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_pairings WHERE id=?1 AND revoked_at IS NULL)",
        [pairing_id],
        |r| r.get(0),
    )?;
    if !known {
        return Ok(false);
    }
    let before = get(conn, pairing_id)?;
    conn.execute(
        "INSERT INTO agent_host_observations
           (pairing_id,client_name,client_version,profile,tasks,skills,events,observed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(pairing_id) DO UPDATE SET client_name=?2,client_version=?3,profile=?4,
           tasks=?5,skills=?6,events=?7,observed_at=?8",
        params![
            pairing_id,
            o.client_name,
            o.client_version,
            o.profile,
            o.tasks,
            o.skills,
            o.events,
            now
        ],
    )?;
    Ok(before.is_none_or(|b| b.observation != *o))
}

pub fn get(conn: &Connection, pairing_id: &str) -> Result<Option<Stored>> {
    Ok(conn
        .query_row(
            "SELECT client_name,client_version,profile,tasks,skills,events,observed_at
             FROM agent_host_observations WHERE pairing_id=?1",
            [pairing_id],
            |r| {
                Ok(Stored {
                    observation: Observation {
                        client_name: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        client_version: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        profile: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        tasks: r.get(3)?,
                        skills: r.get(4)?,
                        events: r.get(5)?,
                    },
                    observed_at: r.get(6)?,
                })
            },
        )
        .optional()?)
}

/// Remove a pairing's observation (when it is revoked).
pub fn delete(conn: &Connection, pairing_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM agent_host_observations WHERE pairing_id=?1",
        [pairing_id],
    )?;
    Ok(())
}
