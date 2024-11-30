// SPDX-FileCopyrightText: 2024 Sebastian Rasor <https://www.sebastianrasor.com/contact>
// SPDX-License-Identifier: AGPL-3.0-only

use anyhow::Result;
use clap::Parser;
use futures::{stream, StreamExt};
use indicatif::{ParallelProgressIterator, ProgressBar, ProgressStyle};
use rayon::prelude::*;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Number;
use tabled::settings::Style;
use tabled::{Table, Tabled};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(short = 'c', long, default_value_t = 8)]
    max_concurrency: usize,

    #[arg(short = 's', long)]
    sport: String,

    #[arg(short, long)]
    league: String,

    #[arg(short = 'S', long)]
    season: u16,

    #[arg(short, long)]
    group: Option<u16>,

    #[arg(short, long)]
    top: Option<usize>,

    #[arg(short, long, default_value_t = false)]
    reverse: bool,

    #[arg(short, long, default_value_t = false, conflicts_with("offense"))]
    defense: bool,

    #[arg(short, long, default_value_t = false, conflicts_with("defense"))]
    offense: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ref {
    #[serde(rename = "$ref")]
    url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PaginatedItems {
    //count: Number,
    page_index: Number,
    //page_size: Number,
    page_count: Number,
    items: Vec<Ref>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Team {
    id: String,
    //display_name: String,
    location: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TeamSchedule {
    team: Team,
    events: Vec<Event>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompetitorScore {
    value: Number,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Competitor {
    id: String,
    score: Option<CompetitorScore>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Competition {
    competitors: Vec<Competitor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    competitions: Vec<Competition>,
}

struct TeamRating {
    name: String,
    defense_rating: f64,
    offense_rating: f64,
}

#[derive(Tabled)]
struct TableEntry {
    #[tabled(rename = "#")]
    rank: usize,
    #[tabled(rename = "Team")]
    team: String,
    #[tabled(rename = "OVR")]
    #[tabled(display_with = "float2")]
    overall_rating: f64,
    #[tabled(rename = "DEF")]
    #[tabled(display_with = "float2")]
    defense_rating: f64,
    #[tabled(rename = "OFF")]
    #[tabled(display_with = "float2")]
    offense_rating: f64,
}

fn float2(n: &f64) -> String {
    format!("{:.2}", n)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let client = Client::new();

    let team_ids = get_team_ids(
        &client,
        args.sport.as_str(),
        args.league.as_str(),
        &args.season,
        args.group.as_ref(),
    )
    .await?;

    let urls: Vec<String> = team_ids
        .par_iter()
        .progress()
        .with_style(ProgressStyle::with_template(
            "{msg} {wide_bar} {pos}/{len}",
        )?)
        .with_message("Generating URLs")
        .map(|team_id| {
            format!(
                "https://site.api.espn.com/apis/site/v2/sports/{}/{}/teams/{}/schedule?season={}",
                args.sport, args.league, team_id, args.season
            )
        })
        .collect();

    let pb = ProgressBar::new(urls.len() as u64);

    let team_schedules: Vec<TeamSchedule> = pb
        .wrap_stream(stream::iter(urls))
        .with_style(ProgressStyle::with_template(
            "{msg} {wide_bar} {pos}/{len}",
        )?)
        .with_message("Fetching scores")
        .map(|url| {
            let client = client.clone();
            tokio::spawn(async move {
                let resp = client.get(url).send().await?;
                resp.json::<TeamSchedule>().await
            })
        })
        .buffer_unordered(args.max_concurrency)
        .filter_map(|x| async {
            match x {
                Ok(Ok(x)) => Some(x),
                _ => None,
            }
        })
        .collect()
        .await;

    let fbs_team_ids: Vec<&str> = team_schedules
        .par_iter()
        .progress()
        .with_style(ProgressStyle::with_template(
            "{msg} {wide_bar} {pos}/{len}",
        )?)
        .with_message("Extracting FBS team IDs")
        .map(|team_schedule| team_schedule.team.id.as_str())
        .collect();

    let team_ratings: Vec<TeamRating> = team_schedules
        .par_iter()
        .progress()
        .with_style(ProgressStyle::with_template(
            "{msg} {wide_bar} {pos}/{len}",
        )?)
        .with_message("Calculating ratings")
        .filter(|team_schedule| team_schedule.events.len() > 0)
        .map(|team_schedule| {
            let mut defense_rating: f64 = 0.0;
            let mut offense_rating: f64 = 0.0;
            let mut count: u8 = 0;
            'events_loop: for event in &team_schedule.events {
                let Some(competition) = event.competitions.last() else {
                    continue 'events_loop;
                };
                let c_index = {
                    if competition.competitors[0].id == team_schedule.team.id {
                        0 as usize
                    } else {
                        1 as usize
                    }
                };
                let competitor = &competition.competitors[c_index];
                let opponent = &competition.competitors[c_index ^ 1];
                if !fbs_team_ids.contains(&opponent.id.as_str()) {
                    continue 'events_loop;
                }
                let Some(competitor_score) = &competitor.score else {
                    continue 'events_loop;
                };
                let Some(opponent_score) = &opponent.score else {
                    continue 'events_loop;
                };
                let Some(competitor_score_f64) = competitor_score.value.as_f64() else {
                    continue 'events_loop;
                };
                let Some(opponent_score_f64) = opponent_score.value.as_f64() else {
                    continue 'events_loop;
                };
                let Some(opponent_team_schedule) = ({
                    let mut opponent_ts: Option<&TeamSchedule> = None;
                    for ts in &team_schedules {
                        if ts.team.id == opponent.id {
                            opponent_ts = Some(ts);
                        }
                    }
                    opponent_ts
                }) else {
                    continue 'events_loop;
                };
                let mut opponent_avg_scored: f64 = 0.0;
                let mut opponent_avg_allowed: f64 = 0.0;
                let mut o_count: u8 = 0;
                'o_events_loop: for o_event in &opponent_team_schedule.events {
                    let Some(o_competition) = o_event.competitions.last() else {
                        continue 'o_events_loop;
                    };
                    let o_c_index = {
                        if o_competition.competitors[0].id == opponent.id {
                            0 as usize
                        } else {
                            1 as usize
                        }
                    };
                    let o_competitor = &o_competition.competitors[o_c_index];
                    let o_opponent = &o_competition.competitors[o_c_index ^ 1];
                    if o_opponent.id == team_schedule.team.id {
                        continue 'o_events_loop;
                    }
                    if !fbs_team_ids.contains(&o_opponent.id.as_str()) {
                        continue 'o_events_loop;
                    }
                    let Some(o_competitor_score) = &o_competitor.score else {
                        continue 'o_events_loop;
                    };
                    let Some(o_opponent_score) = &o_opponent.score else {
                        continue 'o_events_loop;
                    };
                    let Some(o_competitor_score_f64) = o_competitor_score.value.as_f64() else {
                        continue 'o_events_loop;
                    };
                    let Some(o_opponent_score_f64) = o_opponent_score.value.as_f64() else {
                        continue 'o_events_loop;
                    };
                    opponent_avg_allowed += o_opponent_score_f64;
                    opponent_avg_scored += o_competitor_score_f64;
                    o_count += 1;
                }

                opponent_avg_allowed /= o_count as f64;
                opponent_avg_scored /= o_count as f64;

                defense_rating += opponent_avg_scored - opponent_score_f64;
                offense_rating += competitor_score_f64 - opponent_avg_allowed;
                count += 1;
            }

            defense_rating /= count as f64;
            offense_rating /= count as f64;

            return TeamRating {
                name: team_schedule.team.location.clone(),
                defense_rating,
                offense_rating,
            };
        })
        .collect();

    let mut table: Vec<TableEntry> = vec![];

    for rating in &team_ratings {
        table.push(TableEntry {
            rank: 0,
            team: rating.name.clone(),
            overall_rating: rating.defense_rating + rating.offense_rating,
            defense_rating: rating.defense_rating,
            offense_rating: rating.offense_rating,
        })
    }

    table.sort_by(|e1, e2| e1.overall_rating.total_cmp(&e2.overall_rating));

    table.reverse();

    for i in 0..table.len() {
        table[i].rank = i + 1;
    }

    if args.defense {
        table.sort_by(|e1, e2| e1.defense_rating.total_cmp(&e2.defense_rating));
        table.reverse();
    } else if args.offense {
        table.sort_by(|e1, e2| e1.offense_rating.total_cmp(&e2.offense_rating));
        table.reverse();
    }

    if args.reverse {
        table.reverse();
    }

    if args.top.is_some() {
        table.truncate(args.top.unwrap())
    }

    let style = Style::psql();

    println!("{}", Table::new(table).with(style));

    Ok(())
}

async fn get_team_ids(
    client: &Client,
    sport: &str,
    league: &str,
    season: &u16,
    group: Option<&u16>,
) -> Result<Vec<u32>> {
    let mut team_ids: Vec<u32> = vec![];

    let mut page_index = 0;

    loop {
        page_index += 1;
        let url = match group.is_some() {
            true => format!("https://sports.core.api.espn.com/v2/sports/{}/leagues/{}/seasons/{}/types/2/groups/{}/teams?limit=1000&page={}", sport, league, season, group.unwrap(), page_index),
            false => format!("https://sports.core.api.espn.com/v2/sports/{}/leagues/{}/seasons/{}/teams?limit=1000&page={}", sport, league, season, page_index),
        };

        let teams_response = client.get(url).send().await?;
        let teams_response_data = teams_response.json::<PaginatedItems>().await?;

        let mut iteration_team_ids: Vec<u32> = teams_response_data
            .items
            .par_iter()
            .progress()
            .with_style(ProgressStyle::with_template(
                "{msg} {wide_bar} {pos}/{len}",
            )?)
            .with_message("Extracting team IDs")
            .filter_map(|item| {
                let Some(first_split) = item.url.rsplit_once('/') else {
                    return None;
                };
                let Some(second_split) = first_split.1.split_once('?') else {
                    return None;
                };
                let Ok(team_id) = second_split.0.parse::<u32>() else {
                    return None;
                };
                Some(team_id)
            })
            .collect();

        team_ids.append(&mut iteration_team_ids);

        if teams_response_data.page_index == teams_response_data.page_count {
            break;
        }
    }

    Ok(team_ids)
}
