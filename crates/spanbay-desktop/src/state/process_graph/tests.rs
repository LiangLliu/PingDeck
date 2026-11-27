use super::*;

fn node(id: &str, size: (f32, f32), lane: Option<&str>) -> ProcessNodeSpec {
    ProcessNodeSpec {
        id: id.to_owned(),
        archetype: ProcessNodeArchetype::Action,
        status: ProcessNodeStatus::Completed,
        lane_id: lane.map(str::to_owned),
        stage_hint: None,
        size_hint: size,
        primary_port_offset: None,
        ports: Vec::new(),
        is_group_container: false,
        pinned_pos: None,
    }
}

fn ctrl_edge(from: &str, to: &str) -> ProcessEdgeSpec {
    ProcessEdgeSpec {
        id: format!("{from}->{to}"),
        from_node: from.to_owned(),
        from_port: "out".to_owned(),
        to_node: to.to_owned(),
        to_port: "in".to_owned(),
        layer: ProcessEdgeLayer::Control,
        kind: ProcessEdgeKind::Sequence,
        label: None,
        tone: ProcessEdgeTone::Completed,
        dashed: false,
        insertable: true,
    }
}

#[test]
fn heterogeneous_sizes_and_center_alignment_balance_diamond_fanout() {
    let input = ProcessGraphInput {
        nodes: vec![
            node("start", (160.0, 44.0), None),
            node("branch_a", (260.0, 76.0), None),
            node("branch_b", (260.0, 76.0), None),
            node("join", (200.0, 60.0), None),
        ],
        edges: vec![
            ctrl_edge("start", "branch_a"),
            ctrl_edge("start", "branch_b"),
            ctrl_edge("branch_a", "join"),
            ctrl_edge("branch_b", "join"),
        ],
        lanes: Vec::new(),
        policy: LayoutPolicy {
            direction: LayoutDirection::LeftToRight,
            alignment: CrossAlignment::Center,
            swimlanes: false,
            data_flow: DataFlowVisibility::OnFocus,
        },
    };
    let geom = layout_process_graph(&input);
    assert_eq!(geom.stages.len(), 3);
    assert!((geom.stages[0].w - 160.0).abs() < 0.1);
    assert!((geom.stages[1].w - 260.0).abs() < 0.1);
    assert!((geom.stages[2].w - 200.0).abs() < 0.1);

    let start = geom.node("start").expect("start");
    let ba = geom.node("branch_a").expect("branch_a");
    let bb = geom.node("branch_b").expect("branch_b");
    let join = geom.node("join").expect("join");

    let branches_mid_y = (ba.y + bb.y + bb.h) * 0.5;
    let start_mid_y = start.y + start.h * 0.5;
    let join_mid_y = join.y + join.h * 0.5;
    assert!((start_mid_y - branches_mid_y).abs() < 1.0);
    assert!((join_mid_y - branches_mid_y).abs() < 1.0);
}

#[test]
fn multi_output_conditional_ports_distribute_and_emit_branch_labels() {
    let mut gate = node("check_vip", (240.0, 88.0), None);
    gate.archetype = ProcessNodeArchetype::Decision;
    gate.ports = vec![
        ProcessPortSpec::input("in"),
        ProcessPortSpec::branch("true", "VIP", ProcessEdgeTone::Completed),
        ProcessPortSpec::branch("false", "Standard", ProcessEdgeTone::Warning),
    ];

    let mut e_true = ctrl_edge("check_vip", "fast_pay");
    e_true.from_port = "true".to_owned();
    e_true.kind = ProcessEdgeKind::Conditional;
    e_true.label = Some("VIP".to_owned());

    let mut e_false = ctrl_edge("check_vip", "std_pay");
    e_false.from_port = "false".to_owned();
    e_false.kind = ProcessEdgeKind::Conditional;
    e_false.label = Some("Standard".to_owned());
    e_false.tone = ProcessEdgeTone::Skipped;
    e_false.dashed = true;

    let input = ProcessGraphInput {
        nodes: vec![
            gate,
            node("fast_pay", (220.0, 64.0), None),
            node("std_pay", (220.0, 64.0), None),
        ],
        edges: vec![e_true, e_false],
        lanes: Vec::new(),
        policy: LayoutPolicy::default(),
    };
    let geom = layout_process_graph(&input);
    let gate_pos = geom.node("check_vip").expect("gate");
    let p_true = gate_pos.port_pos("true", ProcessPortDirection::Output);
    let p_false = gate_pos.port_pos("false", ProcessPortDirection::Output);
    assert!(
        p_true.1 < p_false.1,
        "conditional output ports must stack top-to-bottom on right edge in LR mode"
    );
    assert!(geom.edges.iter().all(|e| e.label_pos.is_some()));
    assert!(geom.edges.iter().all(|e| e.insert_pos.is_some()));
}

#[test]
fn serpentine_wraps_eight_step_ecommerce_pipeline_into_two_rows_with_turnaround() {
    let names = [
        "register", "login", "browse", "order", "pay", "order_ok", "ship", "complete",
    ];
    let nodes: Vec<ProcessNodeSpec> = names.iter().map(|n| node(n, (230.0, 68.0), None)).collect();
    let mut edges: Vec<ProcessEdgeSpec> = names.windows(2).map(|w| ctrl_edge(w[0], w[1])).collect();
    // Add implicit DataFlow edge from login -> pay ({{token}})
    edges.push(ProcessEdgeSpec {
        id: "df:login->pay".to_owned(),
        from_node: "login".to_owned(),
        from_port: "out".to_owned(),
        to_node: "pay".to_owned(),
        to_port: "in".to_owned(),
        layer: ProcessEdgeLayer::DataFlow,
        kind: ProcessEdgeKind::DataFlow,
        label: Some("{{token}}".to_owned()),
        tone: ProcessEdgeTone::Data,
        dashed: true,
        insertable: false,
    });

    let input = ProcessGraphInput {
        nodes,
        edges,
        lanes: Vec::new(),
        policy: LayoutPolicy {
            direction: LayoutDirection::Serpentine { max_per_row: 4 },
            alignment: CrossAlignment::Center,
            swimlanes: false,
            data_flow: DataFlowVisibility::Always,
        },
    };
    let geom = layout_process_graph(&input);
    let n_order = geom.node("order").expect("order");
    let n_pay = geom.node("pay").expect("pay");
    assert!(
        n_pay.y > n_order.y + n_order.h,
        "step 5 (pay) must wrap onto row 2 below step 4 (order)"
    );
    assert!(
        (n_pay.x - geom.node("register").expect("register").x).abs() < 1.0,
        "row 2 col 0 must align cleanly under row 1 col 0"
    );
    let wrap_edge = geom
        .edges
        .iter()
        .find(|e| e.from_node == "order" && e.to_node == "pay")
        .expect("wrap edge");
    assert_eq!(wrap_edge.waypoints.len(), 4);
    let df_edge = geom
        .edges
        .iter()
        .find(|e| e.layer == ProcessEdgeLayer::DataFlow)
        .expect("dataflow edge");
    assert!(df_edge.label_pos.is_some());
    assert!(df_edge.insert_pos.is_none());
}

#[test]
fn top_to_bottom_and_swimlanes_position_nodes_without_overlap_and_support_hit_testing() {
    let lanes = vec![
        ProcessLaneSpec {
            id: "auth".to_owned(),
            title: "Auth".to_owned(),
            accent_index: 0,
        },
        ProcessLaneSpec {
            id: "order".to_owned(),
            title: "Order".to_owned(),
            accent_index: 1,
        },
        ProcessLaneSpec {
            id: "pay".to_owned(),
            title: "Payment".to_owned(),
            accent_index: 2,
        },
    ];
    let input = ProcessGraphInput {
        nodes: vec![
            node("login", (220.0, 64.0), Some("auth")),
            node("create", (220.0, 64.0), Some("order")),
            node("charge", (220.0, 64.0), Some("pay")),
        ],
        edges: vec![ctrl_edge("login", "create"), ctrl_edge("create", "charge")],
        lanes,
        policy: LayoutPolicy {
            direction: LayoutDirection::LeftToRight,
            alignment: CrossAlignment::Center,
            swimlanes: true,
            data_flow: DataFlowVisibility::OnFocus,
        },
    };
    let geom = layout_process_graph(&input);
    assert_eq!(geom.lanes.len(), 3);
    assert!(geom.lanes[0].y + geom.lanes[0].h <= geom.lanes[1].y);
    assert!(geom.lanes[1].y + geom.lanes[1].h <= geom.lanes[2].y);

    let charge = geom.node("charge").expect("charge");
    assert_eq!(
        hit_test_lane(&geom, charge.x + 10.0, charge.y + 10.0).as_deref(),
        Some("pay")
    );

    // TopToBottom mode sets vertical ports (port_out below port_in).
    let mut tb_input = input.clone();
    tb_input.policy.direction = LayoutDirection::TopToBottom;
    tb_input.policy.swimlanes = false;
    let tb_geom = layout_process_graph(&tb_input);
    let tb_login = tb_geom.node("login").expect("tb login");
    assert!(tb_login.port_out.1 > tb_login.port_in.1);
    assert!(tb_geom.edges.iter().all(|e| !e.horizontal_ports));
}

#[test]
fn hybrid_pinned_override_and_alignment_snap_guides_work_deterministically() {
    let mut n_b = node("b", (220.0, 64.0), None);
    n_b.pinned_pos = Some((480.0, 190.0));
    let input = ProcessGraphInput {
        nodes: vec![node("a", (220.0, 64.0), None), n_b],
        edges: vec![ctrl_edge("a", "b")],
        lanes: Vec::new(),
        policy: LayoutPolicy::default(),
    };
    let geom = layout_process_graph(&input);
    assert!(geom.has_pinned_nodes);
    let b = geom.node("b").expect("b");
    assert!(b.pinned);
    assert!((b.x - 480.0).abs() < 0.1);
    assert!((b.y - 190.0).abs() < 0.1);

    let a = geom.node("a").expect("a");
    let (_sx, sy, guides) = snap_drag_position("b", 480.0, a.y + 3.0, 220.0, 64.0, &geom.nodes);
    assert!(
        (sy - a.y).abs() < 0.1,
        "must snap within 7px to neighbor top"
    );
    assert!(
        guides
            .iter()
            .any(|g| g.orientation == GuideOrientation::Horizontal)
    );
}
