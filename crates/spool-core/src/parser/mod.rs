//! Release-title parsing, ported from Radarr and Sonarr (GPLv3).
pub mod common;
pub mod episode;
pub mod movie;

pub use common::{
    clean_movie_title, clean_series_title, is_video_file, language_from_iso, parse_languages, parse_release_group, remove_file_extension,
};
pub use episode::{parse_episode_path, parse_episode_title, ParsedEpisodeInfo};
pub use movie::{parse_movie_path, parse_movie_title, ParsedMovieInfo};
