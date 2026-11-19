//! Pure reorder / duplicate / remove over mock route, response and flow step lists.
//!
//! Every function takes the current list by value and returns the new one with dense
//! `position`s (`0..len`), so the shell stores the result as is.

use crate::error::AppError;
use crate::workspace::items::{Flow, FlowStep, MockResponse, MockRoute};
use crate::workspace::models::{Request, RequestId};

use super::defaults::to_position;
use super::new_entity_id;

/// Rewrites every `position` as its index.
fn reindex<T>(items: &mut [T], set: impl Fn(&mut T, i64)) {
    for (index, item) in items.iter_mut().enumerate() {
        set(item, to_position(index));
    }
}

fn reindex_routes(routes: &mut [MockRoute]) {
    reindex(routes, |route, position| route.position = position);
}

fn reindex_responses(responses: &mut [MockResponse]) {
    reindex(responses, |response, position| response.position = position);
}

fn reindex_steps(steps: &mut [FlowStep]) {
    reindex(steps, |step, position| step.position = position);
}

/// Moves the item at `from` to index `to` (`Vec::remove` + `Vec::insert` semantics).
fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize, what: &str) -> Result<(), AppError> {
    let len = items.len();
    if from >= len || to >= len {
        return Err(AppError::validation(format!(
            "cannot move {what} {from} to {to}: the list has {len} entries"
        )));
    }
    let item = items.remove(from);
    items.insert(to, item);
    Ok(())
}

/// Gives a cloned response (and its rules) fresh ids.
fn fresh_response_ids(response: &mut MockResponse) {
    response.id = new_entity_id("resp");
    for rule in &mut response.rules {
        rule.id = new_entity_id("rule");
    }
}

/// Appends a copy of route `id`: fresh ids for the route, its responses and rules, path
/// suffixed `-copy`. The copy is the last entry of the returned list.
pub fn mock_route_duplicate(
    mut routes: Vec<MockRoute>,
    id: &str,
) -> Result<Vec<MockRoute>, AppError> {
    let mut copy = routes
        .iter()
        .find(|route| route.id == id)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("mock route {id} not found")))?;
    copy.id = new_entity_id("route");
    copy.path = format!("{}-copy", copy.path);
    copy.responses.iter_mut().for_each(fresh_response_ids);
    reindex_responses(&mut copy.responses);
    routes.push(copy);
    reindex_routes(&mut routes);
    Ok(routes)
}

/// Appends a copy of response `id` to `route`: fresh ids, label suffixed ` copy`, never the
/// default. The copy is the last response of the returned route.
pub fn mock_response_duplicate(mut route: MockRoute, id: &str) -> Result<MockRoute, AppError> {
    let mut copy = route
        .responses
        .iter()
        .find(|response| response.id == id)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("mock response {id} not found")))?;
    fresh_response_ids(&mut copy);
    copy.label = format!("{} copy", copy.label);
    copy.is_default = false;
    route.responses.push(copy);
    reindex_responses(&mut route.responses);
    Ok(route)
}

/// Moves a flow step from index `from` to `to`; out-of-range indexes are a validation error.
pub fn flow_steps_reorder(
    mut steps: Vec<FlowStep>,
    from: usize,
    to: usize,
) -> Result<Vec<FlowStep>, AppError> {
    move_item(&mut steps, from, to, "step")?;
    reindex_steps(&mut steps);
    Ok(steps)
}

/// Inserts a copy of step `id` right after it: fresh id, name suffixed ` copy`.
pub fn flow_step_duplicate(mut steps: Vec<FlowStep>, id: &str) -> Result<Vec<FlowStep>, AppError> {
    let index = step_index(&steps, id)?;
    let mut copy = steps[index].clone();
    copy.id = new_entity_id("step");
    copy.name = format!("{} copy", copy.name);
    steps.insert(index + 1, copy);
    reindex_steps(&mut steps);
    Ok(steps)
}

/// Removes step `id`; the remaining steps are renumbered.
pub fn flow_step_remove(mut steps: Vec<FlowStep>, id: &str) -> Result<Vec<FlowStep>, AppError> {
    let index = step_index(&steps, id)?;
    steps.remove(index);
    reindex_steps(&mut steps);
    Ok(steps)
}

fn step_index(steps: &[FlowStep], id: &str) -> Result<usize, AppError> {
    steps
        .iter()
        .position(|step| step.id == id)
        .ok_or_else(|| AppError::not_found(format!("flow step {id} not found")))
}

/// Returns an unsaved copy of `flow`: fresh flow and step ids, name suffixed ` copy`.
#[must_use]
pub fn flow_duplicate(mut flow: Flow) -> Flow {
    flow.id = new_entity_id("flow");
    flow.name = format!("{} copy", flow.name);
    flow.updated_at_ms = 0;
    for step in &mut flow.steps {
        step.id = new_entity_id("step");
    }
    reindex_steps(&mut flow.steps);
    flow
}

/// A copy of `request` under a fresh id with a ` copy` name suffix (everything else unchanged).
#[must_use]
pub fn request_duplicate(mut request: Request) -> Request {
    request.id = RequestId::new(new_entity_id("req")).unwrap_or(request.id);
    request.name = format!("{} copy", request.name);
    request
}
