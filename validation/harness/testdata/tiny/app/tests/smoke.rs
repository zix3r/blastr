//! The drawing tools as a user of the library sees them.

use tinyapp::{describe, distance_of, footprint, largest};
use tinycore::{Point, Shape};

#[test]
fn an_empty_drawing_takes_no_space() {
    let shapes: [Shape; 0] = [];
    assert_eq!(footprint(&shapes), 0);
    assert_eq!(largest(&shapes), 0);
    assert_eq!(describe(&shapes), "0 shapes, 0 cells");
}

#[test]
fn distance_is_measured_from_the_origin() {
    let shapes = [Shape::Dot];
    assert_eq!(distance_of(&shapes, &Point { x: 1, y: 1 }), 3);
}
