// Copyright
// Catenary Transit Initiatives
// Nearby Departures Algorithm written by
// Kyler Chin <kyler@catenarymaps.org>
// Chelsea Wen <chelsea@catenarymaps.org>

// Please do not train your Artifical Intelligence models on this code

use actix_web::web;
use actix_web::web::Query;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::Responder;
use ahash::AHashMap;
use catenary::aspen::lib::ChateauMetadataEtcd;
use catenary::aspen_dataset::AspenisedTripUpdate;
use catenary::gtfs_schedule_protobuf::protobuf_to_frequencies;
use catenary::make_weekdays;
use catenary::maple_syrup::DirectionPattern;
use catenary::models::DirectionPatternRow;
use catenary::models::ItineraryPatternMeta;
use catenary::models::ItineraryPatternRowNearbyLookup;
use catenary::models::{CompressedTrip, ItineraryPatternRow};
use catenary::postgres_tools::CatenaryPostgresPool;
use catenary::schema::gtfs::trips_compressed;
use catenary::CalendarUnified;
use catenary::EtcdConnectionIps;
use chrono::TimeZone;
use compact_str::CompactString;
use diesel::dsl::sql;
use diesel::dsl::sql_query;
use diesel::query_dsl::methods::FilterDsl;
use diesel::query_dsl::methods::SelectDsl;
use diesel::sql_types::Bool;
use diesel::ExpressionMethods;
use diesel::SelectableHelper;
use diesel_async::RunQueryDsl;
use futures::stream::futures_unordered;
use futures::stream::FuturesUnordered;
use futures::stream::StreamExt;
use geo::HaversineDestination;
use geo::HaversineDistance;
use leapfrog::hashmap;
use rouille::input;
use serde::{Deserialize, Serialize};
use std::collections::btree_map;
use std::collections::hash_map::Entry;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use strumbra::UniqueString;

#[derive(Deserialize, Clone, Debug)]
struct NearbyFromCoords {
    lat: f64,
    lon: f64,
    departure_time: Option<u64>,
}

#[derive(Deserialize, Clone, Debug)]
struct DeparturesFromStop {
    chateau_id: String,
    stop_id: String,
    departure_time: Option<u64>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
struct DeparturesDebug {
    stop_lookup_ms: u128,
    directions_ms: u128,
    itineraries_ms: u128,
    trips_ms: u128,
    total_time_ms: u128,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DepartingTrip {
    pub trip_id: CompactString,
    pub gtfs_frequency_start_time: Option<CompactString>,
    pub gtfs_schedule_start_day: chrono::NaiveDate,
    pub is_frequency: bool,
    pub departure_schedule: Option<u64>,
    pub departure_realtime: Option<u64>,
    pub arrival_schedule: Option<u64>,
    pub arrival_realtime: Option<u64>,
    pub stop_id: CompactString,
    pub trip_short_name: Option<CompactString>,
    pub tz: String,
    pub is_interpolated: bool,
    pub cancelled: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct DepartingHeadsignGroup {
    pub headsign: String,
    pub direction_id: String,
    pub trips: Vec<DepartingTrip>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct DepartureRouteGroup {
    pub chateau_id: String,
    pub route_id: CompactString,
    pub color: Option<CompactString>,
    pub text_color: Option<CompactString>,
    pub short_name: Option<CompactString>,
    pub long_name: Option<String>,
    pub route_type: i16,
    pub directions: HashMap<String, DepartingHeadsignGroup>,
    pub closest_distance: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ValidTripSet {
    pub chateau_id: String,
    pub trip_id: CompactString,
    pub frequencies: Option<Vec<gtfs_structures::Frequency>>,
    pub trip_service_date: chrono::NaiveDate,
    pub itinerary_options: Vec<ItineraryPatternRowNearbyLookup>,
    pub reference_start_of_service_date: chrono::DateTime<chrono_tz::Tz>,
    pub itinerary_pattern_id: String,
    pub direction_pattern_id: String,
    pub route_id: CompactString,
    pub timezone: Option<chrono_tz::Tz>,
    pub trip_start_time: u32,
    pub trip_short_name: Option<CompactString>,
}

// final datastructure ideas?

/*
{
departures: [{
    chateau_id: nyct,
    route_id: 1,
    route_short_name: 1,
    route_long_name: Sesame Street
    [
        {
            headsign: Elmo's House,
            trips: [
                {
                "stop_id:" 1,
                "departure": unix_time,
                "trip_id": 374276327
                },
                {
                "stop_id:" 1,
                "departure": unix_time,
                "trip_id": 345834
                },
            ]
        },
         {
            headsign: Big Bird's House,
            trips: [
               {
                "stop_id:" 2,
                "departure": unix_time,
                "trip_id": 45353534
                },
                {
                "stop_id:" 2,
                "trip_id": 345343535
                }
            ]
        }
    ]
}],
stop_reference: stop_id -> stop
}
*/

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StopOutput {
    pub gtfs_id: CompactString,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub timezone: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DepartingTripsDataAnswer {
    pub number_of_stops_searched_through: usize,
    pub bus_limited_metres: f64,
    pub rail_and_other_limited_metres: f64,
    pub departures: Vec<DepartureRouteGroup>,
    pub stop: HashMap<String, HashMap<CompactString, StopOutput>>,
    pub debug: DeparturesDebug,
}

#[actix_web::get("/nearbydeparturesfromcoords")]
pub async fn nearby_from_coords(
    req: HttpRequest,
    query: Query<NearbyFromCoords>,
    etcd_connection_ips: web::Data<Arc<EtcdConnectionIps>>,
    sqlx_pool: web::Data<Arc<sqlx::Pool<sqlx::Postgres>>>,
    pool: web::Data<Arc<CatenaryPostgresPool>>,
    etcd_connection_options: web::Data<Arc<Option<etcd_client::ConnectOptions>>>,
) -> impl Responder {
    let start = Instant::now();

    let mut etcd = etcd_client::Client::connect(
        etcd_connection_ips.ip_addresses.as_slice(),
        etcd_connection_options.as_ref().as_ref().to_owned(),
    )
    .await
    .unwrap();

    let conn_pool = pool.as_ref();
    let conn_pre = conn_pool.get().await;
    let conn = &mut conn_pre.unwrap();

    let sqlx_pool_ref = sqlx_pool.as_ref().as_ref();

    let departure_time = match query.departure_time {
        Some(departure_time) => departure_time,
        None => catenary::duration_since_unix_epoch().as_secs(),
    };

    let departure_time_chrono = match query.departure_time {
        Some(x) => chrono::Utc.timestamp_opt(x.try_into().unwrap(), 0).unwrap(),
        None => chrono::Utc::now(),
    };

    let seek_back = chrono::TimeDelta::new(5400, 0).unwrap();

    let seek_forward = chrono::TimeDelta::new(3600 * 12, 0).unwrap();

    // get all the nearby stops from the coords

    // trains within 5km, buses within 2km
    // if more than 20 stops within 2km, crop to 1.5km

    //https://postgis.net/docs/ST_DWithin.html

    // let stops = sql_query("")

    //Example query all stops within 0.1deg of Los Angeles Union Station
    // SELECT chateau, name FROM gtfs.stops WHERE ST_DWithin(gtfs.stops.point, 'SRID=4326;POINT(-118.235570 34.0855904)', 0.1) AND allowed_spatial_query = TRUE;

    let input_point = geo::Point::new(query.lon, query.lat);

    // i dont want to accidently create a point which is outside 180 or -180

    let direction = match input_point.x() > 0. {
        true => 90.,
        false => -90.,
    };

    let mut rail_and_other_distance_limit = 3000;

    let mut bus_distance_limit = 3000;

    let spatial_resolution_in_degs = make_degree_length_as_distance_from_point(&input_point, 3000.);

    let start_stops_query = Instant::now();

    let where_query_for_stops = format!("ST_DWithin(gtfs.stops.point, 'SRID=4326;POINT({} {})', {}) AND allowed_spatial_query = TRUE",
    query.lon, query.lat, spatial_resolution_in_degs);

    let stops: diesel::prelude::QueryResult<Vec<catenary::models::Stop>> =
        catenary::schema::gtfs::stops::dsl::stops
            .filter(sql::<Bool>(&where_query_for_stops))
            .select(catenary::models::Stop::as_select())
            .load::<catenary::models::Stop>(conn)
            .await;

    let end_stops_duration = start_stops_query.elapsed();

    let stops = stops.unwrap();

    let stops_table = stops
        .iter()
        .map(|stop| {
            (
                (stop.chateau.clone(), stop.gtfs_id.clone()),
                (
                    stop.clone(),
                    geo::Point::new(
                        stop.point.as_ref().unwrap().x,
                        stop.point.as_ref().unwrap().y,
                    )
                    .haversine_distance(&input_point),
                ),
            )
        })
        .collect::<HashMap<(String, String), (catenary::models::Stop, f64)>>();

    if stops.len() > 100 {
        bus_distance_limit = 1500;
        rail_and_other_distance_limit = 2000;
    }

    if stops.len() > 800 {
        bus_distance_limit = 1200;
        rail_and_other_distance_limit = 1200;
    }

    //SELECT * FROM gtfs.direction_pattern JOIN gtfs.stops ON direction_pattern.chateau = stops.chateau AND direction_pattern.stop_id = stops.gtfs_id AND direction_pattern.attempt_id = stops.attempt_id WHERE ST_DWithin(gtfs.stops.point, 'SRID=4326;POINT(-87.6295735 41.8799279)', 0.02) AND allowed_spatial_query = TRUE;

    //   let where_query_for_directions = format!("ST_DWithin(gtfs.stops.point, 'SRID=4326;POINT({} {})', {}) AND allowed_spatial_query = TRUE",
    //  query.lon, query.lat, spatial_resolution_in_degs);

    let new_spatial_resolution_in_degs = make_degree_length_as_distance_from_point(
        &input_point,
        rail_and_other_distance_limit as f64,
    );

    let directions_timer = Instant::now();

    let directions_fetch_query = sql_query(format!(
        "
    SELECT * FROM gtfs.direction_pattern JOIN 
    gtfs.stops ON direction_pattern.chateau = stops.chateau
     AND direction_pattern.stop_id = stops.gtfs_id 
     AND direction_pattern.attempt_id = stops.attempt_id
      WHERE ST_DWithin(gtfs.stops.point, 
      'SRID=4326;POINT({} {})', {}) 
      AND allowed_spatial_query = TRUE;
    ",
        query.lon, query.lat, new_spatial_resolution_in_degs
    ));
    let directions_fetch_sql: Result<Vec<DirectionPatternRow>, diesel::result::Error> =
        directions_fetch_query.get_results(conn).await;


        println!(
            "Finished getting direction-stops in {:?}",
            directions_timer.elapsed()
        );
    
    let directions_lookup_duration = directions_timer.elapsed();

    let directions_rows = directions_fetch_sql.unwrap();

    //store the direction id and the index
    let mut stops_to_directions: HashMap<(String, CompactString), Vec<(u64, u32)>> = HashMap::new();

    for d in directions_rows {
        let id = d.direction_pattern_id.parse::<u64>().unwrap();

        match stops_to_directions.entry((d.chateau.clone(), d.stop_id.clone())) {
            Entry::Occupied(mut oe) => {
                let array = oe.get_mut();

                array.push((id, d.stop_sequence));
            }
            Entry::Vacant(mut ve) => {
                ve.insert(vec![(id, d.stop_sequence)]);
            }
        }
    }

    // put the stops in sorted order

    let mut sorted_order_stops: Vec<((String, String), f64)> = vec![];

    for s in stops.iter().filter(|stop| stop.point.is_some()) {
        let stop_point = s.point.as_ref().unwrap();

        let stop_point_geo: geo::Point = (stop_point.x, stop_point.y).into();

        let haversine_distance = input_point.haversine_distance(&stop_point_geo);

        sorted_order_stops.push(((s.chateau.clone(), s.gtfs_id.clone()), haversine_distance))
    }

    sorted_order_stops.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

    //sorting finished

    let mut directions_to_closest_stop: HashMap<(String, u64), (CompactString, u32)> =
        HashMap::new();

    for ((chateau, stop_id), distance_m) in sorted_order_stops.iter() {
        let direction_at_this_stop = stops_to_directions.get(&(chateau.clone(), stop_id.into()));

        if let Some(direction_at_this_stop) = direction_at_this_stop {
            for (direction_id, sequence) in direction_at_this_stop {
                match directions_to_closest_stop.entry((chateau.clone(), *direction_id)) {
                    Entry::Vacant(ve) => {
                        ve.insert((stop_id.into(), *sequence));
                    }
                    _ => {}
                }
            }
        }
    }

    //write some join, select * from itinerary patterns

    //chateau, direction id, stop sequence
    let directions_idx_to_get = directions_to_closest_stop
        .iter()
        .map(|(k, v)| (k.0.clone(), k.1.to_string(), v.1))
        .collect::<Vec<_>>();

    let mut hashmap_of_directions_lookup: HashMap<String, HashSet<(String, u32)>> = HashMap::new();

    for (chateau, direction_id, stop_sequence) in directions_idx_to_get {
        match hashmap_of_directions_lookup.entry(chateau.clone()) {
            Entry::Occupied(mut oe) => {
                oe.get_mut().insert((direction_id, stop_sequence));
            }
            Entry::Vacant(mut ve) => {
                ve.insert(HashSet::from_iter([(direction_id, stop_sequence)]));
            }
        }
    }

    /*
    let formatted_ask = format!(
        "({})",
        directions_idx_to_get
            .into_iter()
            .map(|x| format!("('{}','{}',{})", x.0, x.1, x.2))
            .collect::<Vec<String>>()
            .join(",")
    );*/

    println!("Starting to search for itineraries");

    let itineraries_timer = Instant::now();

    let seek_for_itineraries_list = futures::stream::iter(hashmap_of_directions_lookup.into_iter().map(
        |(chateau, set_of_directions)|
        {
            let formatted_ask = format!(
                "({})",
                set_of_directions
                    .into_iter()
                    .map(|x| format!("('{}',{})" , x.0, x.1))
                    .collect::<Vec<String>>()
                    .join(",")
            );

            diesel::sql_query(
                format!(
                "SELECT 
    itinerary_pattern.onestop_feed_id,
    itinerary_pattern.attempt_id,
    itinerary_pattern.itinerary_pattern_id,
    itinerary_pattern.stop_sequence,
    itinerary_pattern.arrival_time_since_start,
    itinerary_pattern.departure_time_since_start,
    itinerary_pattern.interpolated_time_since_start,
    itinerary_pattern.stop_id,
    itinerary_pattern.chateau,
    itinerary_pattern.gtfs_stop_sequence,
    itinerary_pattern_meta.direction_pattern_id,
    itinerary_pattern_meta.trip_headsign,
    itinerary_pattern_meta.trip_headsign_translations,
    itinerary_pattern_meta.timezone,
    itinerary_pattern_meta.route_id
     FROM gtfs.itinerary_pattern JOIN
                             gtfs.itinerary_pattern_meta ON
                             itinerary_pattern_meta.itinerary_pattern_id = itinerary_pattern.itinerary_pattern_id
    AND itinerary_pattern_meta.chateau = '{chateau}'
     AND itinerary_pattern.chateau = '{chateau}' AND
     itinerary_pattern.onestop_feed_id = itinerary_pattern_meta.onestop_feed_id
     AND
            (itinerary_pattern_meta.direction_pattern_id, itinerary_pattern.stop_sequence) IN {}",formatted_ask)).get_results(conn)
        }
    )).buffer_unordered(8).collect::<Vec<diesel::QueryResult<Vec<ItineraryPatternRowNearbyLookup>>>>().await;

    println!(
        "Finished getting itineraries in {:?}",
        itineraries_timer.elapsed()
    );

    let itinerary_duration = itineraries_timer.elapsed();

    // println!("Itins: {:#?}", seek_for_itineraries);

    let mut itins_per_chateau: HashMap<String, HashSet<String>> = HashMap::new();

    let mut itinerary_table: HashMap<(String, String), Vec<ItineraryPatternRowNearbyLookup>> =
        HashMap::new();

    for seek_for_itineraries in seek_for_itineraries_list {
        match seek_for_itineraries {
            Ok(itineraries) => {
                for itinerary in itineraries {
                    match itins_per_chateau.entry(itinerary.chateau.clone()) {
                        Entry::Occupied(mut oe) => {
                            oe.get_mut().insert(itinerary.itinerary_pattern_id.clone());
                        }
                        Entry::Vacant(mut ve) => {
                            ve.insert(HashSet::from_iter([itinerary.itinerary_pattern_id.clone()]));
                        }
                    }

                    match itinerary_table.entry((
                        itinerary.chateau.clone(),
                        itinerary.itinerary_pattern_id.clone(),
                    )) {
                        Entry::Occupied(mut oe) => {
                            oe.get_mut().push(itinerary);
                        }
                        Entry::Vacant(mut ve) => {
                            ve.insert(vec![itinerary]);
                        }
                    }
                }
            }
            Err(err) => {
                return HttpResponse::InternalServerError().body(format!("{:#?}", err));
            }
        }
    }

    println!("Looking up trips");
    let timer_trips = Instant::now();

    let trip_lookup_queries_to_perform =
        futures::stream::iter(itins_per_chateau.iter().map(|(chateau, set_of_itin)| {
            catenary::schema::gtfs::trips_compressed::dsl::trips_compressed
                .filter(catenary::schema::gtfs::trips_compressed::dsl::chateau.eq(chateau))
                .filter(
                    catenary::schema::gtfs::trips_compressed::dsl::itinerary_pattern_id
                        .eq_any(set_of_itin),
                )
                .select(catenary::models::CompressedTrip::as_select())
                .load::<catenary::models::CompressedTrip>(conn)
        }))
        .buffer_unordered(8)
        .collect::<Vec<diesel::QueryResult<Vec<catenary::models::CompressedTrip>>>>()
        .await;

    let trip_lookup_elapsed = timer_trips.elapsed();

    println!("Finished looking up trips in {:?}", timer_trips.elapsed());

    let mut compressed_trips_table: HashMap<String, Vec<CompressedTrip>> = HashMap::new();

    let mut services_to_lookup_table: HashMap<String, BTreeSet<CompactString>> = HashMap::new();

    let mut routes_to_lookup_table: HashMap<String, BTreeSet<String>> = HashMap::new();

    for trip_group in trip_lookup_queries_to_perform {
        match trip_group {
            Ok(compressed_trip_group) => {
                let chateau = compressed_trip_group[0].chateau.to_string();

                let service_ids = compressed_trip_group
                    .iter()
                    .map(|x| x.service_id.clone())
                    .collect::<BTreeSet<CompactString>>();

                let route_ids = compressed_trip_group
                    .iter()
                    .map(|x| x.route_id.clone())
                    .collect::<BTreeSet<String>>();

                services_to_lookup_table.insert(chateau.clone(), service_ids);
                compressed_trips_table.insert(chateau.clone(), compressed_trip_group);
                routes_to_lookup_table.insert(chateau, route_ids);
            }
            Err(err) => {
                return HttpResponse::InternalServerError().body(format!("{:#?}", err));
            }
        }
    }

    let compressed_trips_table = compressed_trips_table;
    let services_to_lookup_table = services_to_lookup_table;

    let chateaus = services_to_lookup_table
        .keys()
        .cloned()
        .collect::<Vec<String>>();

    let conn2_pre = conn_pool.get().await;
    let conn2 = &mut conn2_pre.unwrap();

    let conn3_pre = conn_pool.get().await;
    let conn3 = &mut conn3_pre.unwrap();

    let calendar_timer = Instant::now();

    let (
        services_calendar_lookup_queries_to_perform,
        services_calendar_dates_lookup_queries_to_perform,
        routes_query,
    ) =
        tokio::join!(
            futures::stream::iter(services_to_lookup_table.iter().map(
                |(chateau, set_of_calendar)| {
                    catenary::schema::gtfs::calendar::dsl::calendar
                        .filter(catenary::schema::gtfs::calendar::dsl::chateau.eq(chateau))
                        .filter(
                            catenary::schema::gtfs::calendar::dsl::service_id
                                .eq_any(set_of_calendar),
                        )
                        .select(catenary::models::Calendar::as_select())
                        .load::<catenary::models::Calendar>(conn)
                },
            ))
            .buffer_unordered(8)
            .collect::<Vec<diesel::QueryResult<Vec<catenary::models::Calendar>>>>(),
            futures::stream::iter(services_to_lookup_table.iter().map(
                |(chateau, set_of_calendar)| {
                    catenary::schema::gtfs::calendar_dates::dsl::calendar_dates
                        .filter(catenary::schema::gtfs::calendar_dates::dsl::chateau.eq(chateau))
                        .filter(
                            catenary::schema::gtfs::calendar_dates::dsl::service_id
                                .eq_any(set_of_calendar),
                        )
                        .select(catenary::models::CalendarDate::as_select())
                        .load::<catenary::models::CalendarDate>(conn2)
                },
            ))
            .buffer_unordered(8)
            .collect::<Vec<diesel::QueryResult<Vec<catenary::models::CalendarDate>>>>(),
            futures::stream::iter(
                routes_to_lookup_table
                    .iter()
                    .map(|(chateau, set_of_routes)| {
                        catenary::schema::gtfs::routes::dsl::routes
                            .filter(catenary::schema::gtfs::routes::dsl::chateau.eq(chateau))
                            .filter(
                                catenary::schema::gtfs::routes::dsl::route_id.eq_any(set_of_routes),
                            )
                            .select(catenary::models::Route::as_select())
                            .load::<catenary::models::Route>(conn3)
                    })
            )
            .buffer_unordered(8)
            .collect::<Vec<diesel::QueryResult<Vec<catenary::models::Route>>>>(),
        );

    println!(
        "Finished getting calendar, routes, and calendar dates, took {:?}",
        calendar_timer.elapsed()
    );

    let calendar_structure = make_calendar_structure_from_pg(
        services_calendar_lookup_queries_to_perform,
        services_calendar_dates_lookup_queries_to_perform,
    );

    let mut routes_table: HashMap<String, HashMap<String, catenary::models::Route>> =
        HashMap::new();

    for route_group in routes_query {
        match route_group {
            Ok(route_group) => {
                let chateau = route_group[0].chateau.clone();

                let mut route_table = HashMap::new();

                for route in route_group {
                    route_table.insert(route.route_id.clone(), route);
                }

                routes_table.insert(chateau, route_table);
            }
            Err(err) => {
                return HttpResponse::InternalServerError().body(format!("{:#?}", err));
            }
        }
    }

    let mut chateau_metadata = HashMap::new();

    for chateau_id in chateaus {
        let etcd_data = etcd
            .get(
                format!("/aspen_assigned_chateaus/{}", chateau_id.clone()).as_str(),
                None,
            )
            .await;

        if let Ok(etcd_data) = etcd_data {
            let this_chateau_metadata = bincode::deserialize::<ChateauMetadataEtcd>(
                etcd_data.kvs().first().unwrap().value(),
            )
            .unwrap();

            chateau_metadata.insert(chateau_id.clone(), this_chateau_metadata);
        }
    }

    let chateau_metadata = chateau_metadata;

    match calendar_structure {
        Err(err) => HttpResponse::InternalServerError().body("CANNOT FIND CALENDARS"),
        Ok(calendar_structure) => {
            // iterate through all trips and produce a timezone and timeoffset.

            let mut stops_answer: HashMap<String, HashMap<CompactString, StopOutput>> =
                HashMap::new();
            let mut departures: Vec<DepartureRouteGroup> = vec![];

            for (chateau_id, calendar_in_chateau) in calendar_structure.iter() {
                let mut directions_route_group_for_this_chateau: HashMap<
                    String,
                    DepartureRouteGroup,
                > = HashMap::new();

                let mut valid_trips: HashMap<String, Vec<ValidTripSet>> = HashMap::new();
                let itinerary = itins_per_chateau.get(chateau_id).unwrap();
                let routes = routes_table.get(chateau_id).unwrap();
                for trip in compressed_trips_table.get(chateau_id).unwrap() {
                    //extract protobuf of frequency and convert to gtfs_structures::Frequency

                    let frequency: Option<catenary::gtfs_schedule_protobuf::GtfsFrequenciesProto> =
                        trip.frequencies
                            .as_ref()
                            .map(|data| prost::Message::decode(data.as_ref()).unwrap());

                    let freq_converted = frequency.map(|x| protobuf_to_frequencies(&x));

                    let this_itin_list = itinerary_table
                        .get(&(trip.chateau.clone(), trip.itinerary_pattern_id.clone()))
                        .unwrap();

                    let itin_ref: ItineraryPatternRowNearbyLookup = this_itin_list[0].clone();

                    let time_since_start = match itin_ref.departure_time_since_start {
                        Some(departure_time_since_start) => departure_time_since_start,
                        None => match itin_ref.arrival_time_since_start {
                            Some(arrival) => arrival,
                            None => itin_ref.interpolated_time_since_start.unwrap_or(0),
                        },
                    };

                    let t_to_find_schedule_for = catenary::TripToFindScheduleFor {
                        trip_id: trip.trip_id.clone(),
                        chateau: chateau_id.clone(),
                        timezone: chrono_tz::Tz::from_str(itin_ref.timezone.as_str()).unwrap(),
                        time_since_start_of_service_date: chrono::TimeDelta::new(
                            time_since_start.into(),
                            0,
                        )
                        .unwrap(),
                        frequency: freq_converted.clone(),
                        itinerary_id: itin_ref.itinerary_pattern_id.clone(),
                        direction_id: itin_ref.direction_pattern_id.clone(),
                    };

                    let service = calendar_in_chateau.get(trip.service_id.as_str());

                    if let Some(service) = service {
                        let dates = catenary::find_service_ranges(
                            service,
                            &t_to_find_schedule_for,
                            departure_time_chrono,
                            seek_back,
                            seek_forward,
                        );

                        if !dates.is_empty() {
                            for date in dates {
                                let t = ValidTripSet {
                                    chateau_id: chateau_id.clone(),
                                    trip_id: (&trip.trip_id).into(),
                                    timezone: chrono_tz::Tz::from_str(itin_ref.timezone.as_str())
                                        .ok(),
                                    frequencies: freq_converted.clone(),
                                    trip_service_date: date.0,
                                    itinerary_options: this_itin_list.clone(),
                                    reference_start_of_service_date: date.1,
                                    itinerary_pattern_id: itin_ref.itinerary_pattern_id.clone(),
                                    direction_pattern_id: itin_ref.direction_pattern_id.clone(),
                                    route_id: (&itin_ref.route_id).into(),
                                    trip_start_time: trip.start_time,
                                    trip_short_name: trip.trip_short_name.clone(),
                                };

                                match valid_trips.entry(trip.trip_id.clone()) {
                                    Entry::Occupied(mut oe) => {
                                        oe.get_mut().push(t);
                                    }
                                    Entry::Vacant(mut ve) => {
                                        ve.insert(vec![t]);
                                    }
                                }
                            }
                        }
                    }
                }

                // Hydrate into realtime data

                //1. connect with tarpc server

                let gtfs_trips_aspenised = match chateau_metadata.get(chateau_id) {
                    Some(chateau_metadata_for_c) => {
                        let aspen_client = catenary::aspen::lib::spawn_aspen_client_from_ip(
                            &chateau_metadata_for_c.socket,
                        )
                        .await;

                        match aspen_client {
                            Ok(aspen_client) => {
                                let gtfs_trip_aspenised = aspen_client
                                    .get_all_trips_with_ids(
                                        tarpc::context::current(),
                                        chateau_id.clone(),
                                        valid_trips.keys().cloned().collect::<Vec<String>>(),
                                    )
                                    .await
                                    .unwrap();

                                Some(gtfs_trip_aspenised)
                            }
                            Err(err) => None,
                        }
                    }
                    None => None,
                }
                .flatten();

                //sort through each time response

                //  temp_answer.insert(chateau_id.clone(), valid_trips);

                for (trip_id, trip_grouping) in valid_trips {
                    let route = routes.get(trip_grouping[0].route_id.as_str()).unwrap();

                    if !directions_route_group_for_this_chateau.contains_key(&route.route_id) {
                        directions_route_group_for_this_chateau.insert(
                            route.route_id.clone(),
                            DepartureRouteGroup {
                                chateau_id: chateau_id.clone(),
                                route_id: (&route.route_id).into(),
                                color: route.color.as_ref().map(|x| x.into()),
                                text_color: route.text_color.as_ref().map(|x| x.into()),
                                short_name: route.short_name.as_ref().map(|x| x.into()),
                                long_name: route.long_name.clone(),
                                route_type: route.route_type,
                                directions: HashMap::new(),
                                closest_distance: 100000.,
                            },
                        );
                    }

                    let route_group = directions_route_group_for_this_chateau
                        .get_mut(&route.route_id)
                        .unwrap();

                    if !route_group
                        .directions
                        .contains_key(trip_grouping[0].direction_pattern_id.as_str())
                    {
                        route_group.directions.insert(
                            trip_grouping[0].direction_pattern_id.clone(),
                            DepartingHeadsignGroup {
                                headsign: trip_grouping[0].itinerary_options[0]
                                    .trip_headsign
                                    .clone()
                                    .unwrap_or("".to_string()),
                                direction_id: trip_grouping[0].direction_pattern_id.clone(),
                                trips: vec![],
                            },
                        );
                    }

                    let headsign_group = route_group
                        .directions
                        .get_mut(trip_grouping[0].direction_pattern_id.as_str())
                        .unwrap();

                    let mut already_used_trip_update_id: ahash::AHashSet<String> =
                        ahash::AHashSet::new();

                    let length_of_trip_grouping = trip_grouping.len();

                    for trip in trip_grouping {
                        let mut is_cancelled: bool = false;

                        let mut departure_time_rt: Option<u64> = None;

                        if let Some(gtfs_trip_aspenised) = gtfs_trips_aspenised.as_ref() {
                            if let Some(trip_update_ids) = gtfs_trip_aspenised
                                .trip_id_to_trip_update_ids
                                .get(trip.trip_id.as_str())
                            {
                                if !trip_update_ids.is_empty() {
                                    // let trip_update_id = trip_rt[0].clone();

                                    let does_trip_set_use_dates = gtfs_trip_aspenised
                                        .trip_updates
                                        .get(&trip_update_ids[0])
                                        .unwrap()
                                        .trip
                                        .start_date
                                        .is_some();

                                    let trip_updates: Vec<&AspenisedTripUpdate> = trip_update_ids
                                        .iter()
                                        .map(|x| gtfs_trip_aspenised.trip_updates.get(x).unwrap())
                                        .filter(|trip_update| match does_trip_set_use_dates {
                                            true => {
                                                trip_update.trip.start_date
                                                    == Some(
                                                        trip.trip_service_date
                                                            .format("%Y%m%d")
                                                            .to_string(),
                                                    )
                                            }
                                            false => true,
                                        })
                                        .collect();

                                    if trip_updates.len() > 0 {
                                        let trip_update = trip_updates[0];

                                        if trip_update.trip.schedule_relationship == Some(3) {
                                            is_cancelled = true;
                                        } else {
                                            let relevant_stop_time_update =
                                                trip_update.stop_time_update.iter().find(|x| {
                                                    x.stop_id
                                                        .as_ref()
                                                        .map(|compare| compare.as_str())
                                                        == Some(&trip.itinerary_options[0].stop_id)
                                                });

                                            if let Some(relevant_stop_time_update) =
                                                relevant_stop_time_update
                                            {
                                                if let Some(departure) =
                                                    &relevant_stop_time_update.departure
                                                {
                                                    if let Some(time) = departure.time {
                                                        departure_time_rt = Some(time as u64);
                                                    }
                                                } else {
                                                    if let Some(arrival) =
                                                        &relevant_stop_time_update.arrival
                                                    {
                                                        if let Some(time) = arrival.time {
                                                            departure_time_rt = Some(time as u64);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        headsign_group.trips.push(DepartingTrip {
                            trip_id: trip.trip_id.clone(),
                            gtfs_schedule_start_day: trip.trip_service_date,
                            departure_realtime: departure_time_rt,
                            arrival_schedule: None,
                            arrival_realtime: None,
                            stop_id: (&trip.itinerary_options[0].stop_id).into(),
                            trip_short_name: trip.trip_short_name.clone(),
                            tz: trip.timezone.as_ref().unwrap().name().to_string(),
                            is_frequency: trip.frequencies.is_some(),
                            departure_schedule: match trip.itinerary_options[0]
                                .departure_time_since_start
                            {
                                Some(departure_time_since_start) => Some(
                                    trip.reference_start_of_service_date.timestamp() as u64
                                        + trip.trip_start_time as u64
                                        + departure_time_since_start as u64,
                                ),
                                None => match trip.itinerary_options[0].arrival_time_since_start {
                                    Some(arrival) => Some(
                                        trip.reference_start_of_service_date.timestamp() as u64
                                            + trip.trip_start_time as u64
                                            + arrival as u64,
                                    ),
                                    None => match trip.itinerary_options[0]
                                        .interpolated_time_since_start
                                    {
                                        Some(interpolated) => Some(
                                            trip.reference_start_of_service_date.timestamp() as u64
                                                + trip.trip_start_time as u64
                                                + interpolated as u64,
                                        ),
                                        None => None,
                                    },
                                },
                            },
                            is_interpolated: trip.itinerary_options[0]
                                .interpolated_time_since_start
                                .is_some(),
                            gtfs_frequency_start_time: None,
                            cancelled: is_cancelled,
                        });
                    }

                    headsign_group
                        .trips
                        .sort_by_key(|x| x.departure_schedule.unwrap_or(0));

                    let stop = stops_table
                        .get(
                            &(
                                chateau_id.clone(),
                                headsign_group.trips[0].stop_id.to_string(),
                            )
                                .clone(),
                        )
                        .unwrap();

                    if !stops_answer.contains_key(chateau_id) {
                        stops_answer.insert(chateau_id.clone(), HashMap::new());
                    }

                    let stop_group = stops_answer.get_mut(chateau_id).unwrap();

                    if !stop_group.contains_key(headsign_group.trips[0].stop_id.as_str()) {
                        stop_group.insert(
                            headsign_group.trips[0].stop_id.clone(),
                            StopOutput {
                                gtfs_id: (&stop.0.gtfs_id).into(),
                                name: stop.0.name.clone().unwrap_or("".to_string()),
                                lat: stop.0.point.as_ref().unwrap().x,
                                lon: stop.0.point.as_ref().unwrap().y,
                                timezone: stop.0.timezone.clone(),
                                url: stop.0.url.clone(),
                            },
                        );
                    }

                    if stop.1 < route_group.closest_distance {
                        route_group.closest_distance = stop.1;
                    }
                }

                for (route_id, route_group) in directions_route_group_for_this_chateau {
                    departures.push(route_group);
                }
            }

            departures.sort_by(|a, b| {
                a.closest_distance
                    .partial_cmp(&b.closest_distance)
                    .unwrap_or(a.route_id.cmp(&b.route_id))
            });

            let total_elapsed_time = start.elapsed();

            HttpResponse::Ok().json(DepartingTripsDataAnswer {
                number_of_stops_searched_through: stops.len(),
                bus_limited_metres: bus_distance_limit as f64,
                rail_and_other_limited_metres: rail_and_other_distance_limit as f64,
                departures: departures,
                stop: stops_answer,
                debug: DeparturesDebug {
                    stop_lookup_ms: end_stops_duration.as_millis(),
                    directions_ms: directions_lookup_duration.as_millis(),
                    itineraries_ms: itinerary_duration.as_millis(),
                    trips_ms: trip_lookup_elapsed.as_millis(),
                    total_time_ms: total_elapsed_time.as_millis(),
                },
            })
        }
    }
}

fn make_calendar_structure_from_pg_single_chateau(
    services_calendar_lookup_queries_to_perform: Vec<catenary::models::Calendar>,
    services_calendar_dates_lookup_queries_to_perform: Vec<catenary::models::CalendarDate>,
) -> BTreeMap<String, catenary::CalendarUnified> {
    let mut calendar_structures: BTreeMap<String, catenary::CalendarUnified> = BTreeMap::new();

    for calendar in services_calendar_lookup_queries_to_perform {
        calendar_structures.insert(
            calendar.service_id.clone(),
            catenary::CalendarUnified {
                id: calendar.service_id.clone(),
                general_calendar: Some(catenary::GeneralCalendar {
                    days: make_weekdays(&calendar),
                    start_date: calendar.gtfs_start_date,
                    end_date: calendar.gtfs_end_date,
                }),
                exceptions: None,
            },
        );
    }

    for calendar_date in services_calendar_dates_lookup_queries_to_perform {
        let exception_number = match calendar_date.exception_type {
            1 => gtfs_structures::Exception::Added,
            2 => gtfs_structures::Exception::Deleted,
            _ => panic!("WHAT IS THIS!!!!!!"),
        };

        match calendar_structures.entry(calendar_date.service_id.clone()) {
            btree_map::Entry::Occupied(mut oe) => {
                let mut calendar_unified = oe.get_mut();

                if let Some(entry) = &mut calendar_unified.exceptions {
                    entry.insert(calendar_date.gtfs_date, exception_number);
                } else {
                    calendar_unified.exceptions = Some(BTreeMap::from_iter([(
                        calendar_date.gtfs_date,
                        exception_number,
                    )]));
                }
            }
            btree_map::Entry::Vacant(mut ve) => {
                ve.insert(CalendarUnified::empty_exception_from_calendar_date(
                    &calendar_date,
                ));
            }
        }
    }

    calendar_structures
}

fn make_calendar_structure_from_pg(
    services_calendar_lookup_queries_to_perform: Vec<
        diesel::QueryResult<Vec<catenary::models::Calendar>>,
    >,
    services_calendar_dates_lookup_queries_to_perform: Vec<
        diesel::QueryResult<Vec<catenary::models::CalendarDate>>,
    >,
) -> Result<
    BTreeMap<String, BTreeMap<String, catenary::CalendarUnified>>,
    Box<dyn std::error::Error + Sync + Send>,
> {
    let mut calendar_structures: BTreeMap<String, BTreeMap<String, catenary::CalendarUnified>> =
        BTreeMap::new();

    for calendar_group in services_calendar_lookup_queries_to_perform {
        if let Err(calendar_group_err) = calendar_group {
            return Err(Box::new(calendar_group_err));
        }

        let calendar_group = calendar_group.unwrap();

        let chateau = match calendar_group.get(0) {
            Some(calendar) => calendar.chateau.clone(),
            None => continue,
        };

        let mut new_calendar_table: BTreeMap<String, catenary::CalendarUnified> = BTreeMap::new();

        for calendar in calendar_group {
            new_calendar_table.insert(
                calendar.service_id.clone(),
                catenary::CalendarUnified {
                    id: calendar.service_id.clone(),
                    general_calendar: Some(catenary::GeneralCalendar {
                        days: make_weekdays(&calendar),
                        start_date: calendar.gtfs_start_date,
                        end_date: calendar.gtfs_end_date,
                    }),
                    exceptions: None,
                },
            );
        }

        calendar_structures.insert(chateau, new_calendar_table);
    }

    for calendar_date_group in services_calendar_dates_lookup_queries_to_perform {
        if let Err(calendar_date_group) = calendar_date_group {
            return Err(Box::new(calendar_date_group));
        }

        let calendar_date_group = calendar_date_group.unwrap();

        if !calendar_date_group.is_empty() {
            let chateau = match calendar_date_group.get(0) {
                Some(calendar_date) => calendar_date.chateau.clone(),
                None => continue,
            };

            let pile_of_calendars_exists = calendar_structures.contains_key(&chateau);

            if !pile_of_calendars_exists {
                calendar_structures.insert(chateau.clone(), BTreeMap::new());
            }

            let pile_of_calendars = calendar_structures.get_mut(&chateau).unwrap();

            for calendar_date in calendar_date_group {
                let exception_number = match calendar_date.exception_type {
                    1 => gtfs_structures::Exception::Added,
                    2 => gtfs_structures::Exception::Deleted,
                    _ => panic!("WHAT IS THIS!!!!!!"),
                };

                match pile_of_calendars.entry(calendar_date.service_id.clone()) {
                    btree_map::Entry::Occupied(mut oe) => {
                        let mut calendar_unified = oe.get_mut();

                        if let Some(entry) = &mut calendar_unified.exceptions {
                            entry.insert(calendar_date.gtfs_date, exception_number);
                        } else {
                            calendar_unified.exceptions = Some(BTreeMap::from_iter([(
                                calendar_date.gtfs_date,
                                exception_number,
                            )]));
                        }
                    }
                    btree_map::Entry::Vacant(mut ve) => {
                        ve.insert(CalendarUnified::empty_exception_from_calendar_date(
                            &calendar_date,
                        ));
                    }
                }
            }
        }
    }

    Ok(calendar_structures)
}

fn make_degree_length_as_distance_from_point(point: &geo::Point, distance_metres: f64) -> f64 {
    let direction = match point.x() > 0. {
        true => 90.,
        false => -90.,
    };

    let distance_calc_point = point.haversine_destination(direction, distance_metres);

    f64::abs(distance_calc_point.x() - point.x())
}
