-- Your SQL goes here
CREATE INDEX chateau_itin_pattern_stop_id_idx ON gtfs.itinerary_pattern (chateau, stop_id);