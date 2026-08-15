use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use geo_types::{
    Coord, Geometry, GeometryCollection, LineString, MultiLineString, MultiPoint, Point,
};
use osmdb::{Database, DatabaseReader, LocationStoreReader, MemberType, Relation, Way};

#[derive(Debug, Clone, Copy)]
pub enum RelationGeometryKind {
    MultiPoint,
    MultiLineString,
    GeometryCollection,
}

#[derive(Default)]
struct RelationParts {
    points: Vec<Point<f64>>,
    lines: Vec<LineString<f64>>,
    collection: Vec<Geometry<f64>>,
}

pub struct GeometryResolver {
    database: Arc<Database>,
    locations: LocationStoreReader,
}

impl GeometryResolver {
    pub fn new(database: Arc<Database>, location_path: &std::path::Path) -> Result<Self> {
        let locations = LocationStoreReader::open(location_path).with_context(|| {
            format!(
                "failed to open location store '{}'",
                location_path.display()
            )
        })?;
        Ok(Self {
            database,
            locations,
        })
    }

    pub fn point(&self, node_id: i64) -> Result<Point<f64>> {
        let location = self
            .locations
            .get(node_id)
            .ok_or_else(|| anyhow::anyhow!("location for node {node_id} is missing"))?;
        Ok(Point::new(
            location.longitude_degrees(),
            location.latitude_degrees(),
        ))
    }

    pub fn way_geometry(&self, way_id: i64, way: &Way) -> Result<LineString<f64>> {
        if way.node_ids.len() < 2 {
            bail!("way {way_id} has fewer than two nodes");
        }
        let mut coordinates = Vec::with_capacity(way.node_ids.len());
        for node_id in &way.node_ids {
            let point = self.point(*node_id)?;
            coordinates.push(Coord {
                x: point.x(),
                y: point.y(),
            });
        }
        Ok(LineString::new(coordinates))
    }

    pub fn relation_geometry(
        &self,
        relation_id: i64,
        relation: &Relation,
        kind: RelationGeometryKind,
    ) -> Result<Geometry<f64>> {
        let mut stack = HashSet::new();
        stack.insert(relation_id);
        let mut parts = RelationParts::default();
        self.collect_relation(relation_id, relation, kind, &mut stack, &mut parts)?;

        match kind {
            RelationGeometryKind::MultiPoint if parts.points.is_empty() => {
                bail!("relation {relation_id} contains no node geometry")
            }
            RelationGeometryKind::MultiLineString if parts.lines.is_empty() => {
                bail!("relation {relation_id} contains no way geometry")
            }
            RelationGeometryKind::GeometryCollection if parts.collection.is_empty() => {
                bail!("relation {relation_id} contains no supported geometry")
            }
            RelationGeometryKind::MultiPoint => {
                Ok(Geometry::MultiPoint(MultiPoint::new(parts.points)))
            }
            RelationGeometryKind::MultiLineString => {
                Ok(Geometry::MultiLineString(MultiLineString::new(parts.lines)))
            }
            RelationGeometryKind::GeometryCollection => Ok(Geometry::GeometryCollection(
                GeometryCollection::new_from(parts.collection),
            )),
        }
    }

    fn collect_relation(
        &self,
        relation_id: i64,
        relation: &Relation,
        kind: RelationGeometryKind,
        stack: &mut HashSet<i64>,
        parts: &mut RelationParts,
    ) -> Result<()> {
        let reader = DatabaseReader::new(self.database.inner());
        for member in &relation.members {
            match member.member_type {
                MemberType::Node
                    if matches!(
                        kind,
                        RelationGeometryKind::MultiPoint | RelationGeometryKind::GeometryCollection
                    ) =>
                {
                    let point = self.point(member.id).with_context(|| {
                        format!(
                            "while resolving node member {} of relation {relation_id}",
                            member.id
                        )
                    })?;
                    if matches!(kind, RelationGeometryKind::GeometryCollection) {
                        parts.collection.push(Geometry::Point(point));
                    } else {
                        parts.points.push(point);
                    }
                }
                MemberType::Way
                    if matches!(
                        kind,
                        RelationGeometryKind::MultiLineString
                            | RelationGeometryKind::GeometryCollection
                    ) =>
                {
                    let way = reader.get_way(member.id)?.ok_or_else(|| {
                        anyhow::anyhow!(
                            "way member {} of relation {relation_id} is missing",
                            member.id
                        )
                    })?;
                    let line = self.way_geometry(member.id, &way)?;
                    if matches!(kind, RelationGeometryKind::GeometryCollection) {
                        parts.collection.push(Geometry::LineString(line));
                    } else {
                        parts.lines.push(line);
                    }
                }
                MemberType::Relation => {
                    if !stack.insert(member.id) {
                        bail!(
                            "cycle detected at nested relation {} while resolving relation {relation_id}",
                            member.id
                        );
                    }
                    let nested = reader.get_relation(member.id)?.ok_or_else(|| {
                        anyhow::anyhow!(
                            "relation member {} of relation {relation_id} is missing",
                            member.id
                        )
                    })?;
                    self.collect_relation(member.id, &nested, kind, stack, parts)?;
                    stack.remove(&member.id);
                }
                _ => {}
            }
        }
        Ok(())
    }
}
