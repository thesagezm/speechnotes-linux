import Foundation
import SwiftCrossUI

/// A multi-branch switch that keeps every branch's view graph node alive
/// once created. SwiftCrossUI's `if/else` (EitherView) destroys the inactive
/// branch and rebuilds its entire widget tree on every switch — hundreds of
/// milliseconds per pane change for settings-sized trees. This view pays
/// each branch's construction exactly once, then switches by swapping which
/// child widget is mounted (an O(1) widget move).
///
/// Branch views ride in `AnyView` wrappers; each slot must always render the
/// same concrete view type (true for the app's panes — slot identity is
/// fixed by the caller), so AnyView's in-place update path holds and no node
/// is ever rebuilt after its first activation.
struct CachedSwitchView: View {
    typealias Children = CachedSwitchChildren

    var activeIndex: Int
    var branches: [AnyView]

    var body: some View {
        EmptyView()
    }

    func children<Backend: BaseAppBackend>(
        backend: Backend,
        snapshots: [ViewGraphSnapshotter.NodeSnapshot]?,
        environment: EnvironmentValues
    ) -> any ViewGraphNodeChildren {
        return CachedSwitchChildren(
            from: self,
            backend: backend,
            snapshots: snapshots,
            environment: environment
        )
    }

    func asWidget<Backend: BaseAppBackend>(
        _ children: any ViewGraphNodeChildren,
        backend: Backend
    ) -> Backend.Widget {
        let children = children as! CachedSwitchChildren
        let container = backend.createContainer()
        backend.insert(
            children.mountedNode().getWidget().into(),
            into: container,
            at: 0
        )
        backend.setPosition(ofChildAt: 0, in: container, to: .zero)
        return container
    }

    func computeLayout<Backend: BaseAppBackend>(
        _ widget: Backend.Widget,
        children: any ViewGraphNodeChildren,
        proposedSize: ProposedViewSize,
        environment: EnvironmentValues,
        backend: Backend
    ) -> ViewLayoutResult {
        let children = children as! CachedSwitchChildren
        // Update the active branch in place (creating its node on first
        // activation). Inactive branches are skipped entirely — that's the
        // whole point: a switch measures one pane, not rebuilds five.
        if children.node(at: activeIndex) == nil {
            children.rebuild(
                at: activeIndex,
                view: branches[activeIndex],
                backend: backend,
                snapshot: children.snapshot(at: activeIndex),
                environment: environment
            )
        }
        var (matched, result) = children.node(at: activeIndex)!.computeLayoutWithNewView(
            branches[activeIndex],
            proposedSize,
            environment
        )
        if !matched {
            // Slot types are stable by contract; a mismatch means the caller
            // reordered slots — rebuild rather than render garbage.
            children.rebuild(
                at: activeIndex,
                view: branches[activeIndex],
                backend: backend,
                snapshot: nil,
                environment: environment
            )
            let (_, newResult) = children.node(at: activeIndex)!.computeLayoutWithNewView(
                branches[activeIndex],
                proposedSize,
                environment
            )
            result = newResult
        }

        if children.mountedIndex != activeIndex {
            children.widgetNeedsReinsertion = true
        }
        return result
    }

    func commit<Backend: BaseAppBackend>(
        _ widget: Backend.Widget,
        children: any ViewGraphNodeChildren,
        layout: ViewLayoutResult,
        environment: EnvironmentValues,
        backend: Backend
    ) {
        let children = children as! CachedSwitchChildren
        if children.widgetNeedsReinsertion || children.mountedIndex != activeIndex {
            backend.remove(childAt: 0, from: widget)
            backend.insert(
                children.mountedNode().getWidget().into(),
                into: widget,
                at: 0
            )
            backend.setPosition(ofChildAt: 0, in: widget, to: .zero)
            children.mountedIndex = activeIndex
            children.widgetNeedsReinsertion = false
        }

        _ = children.node(at: activeIndex)?.commit()

        backend.setSize(
            of: widget,
            to: SIMD2(Int(layout.size.width.rounded()), Int(layout.size.height.rounded()))
        )
    }
}

/// The kept-alive per-branch nodes for ``CachedSwitchView``.
@MainActor
class CachedSwitchChildren: ViewGraphNodeChildren {
    private var nodes: [ErasedViewGraphNode?]
    private var slotSnapshots: [ViewGraphSnapshotter.NodeSnapshot?]
    /// The branch whose widget is currently inside the parent container.
    var mountedIndex: Int
    var widgetNeedsReinsertion = false

    var widgets: [AnyWidget] {
        [mountedNode().getWidget()]
    }

    var erasedNodes: [ErasedViewGraphNode] {
        nodes.compactMap { $0 }
    }

    init(
        from view: CachedSwitchView,
        backend: some BaseAppBackend,
        snapshots: [ViewGraphSnapshotter.NodeSnapshot]?,
        environment: EnvironmentValues
    ) {
        nodes = Array(repeating: nil, count: view.branches.count)
        slotSnapshots = Array(repeating: nil, count: view.branches.count)
        mountedIndex = view.activeIndex
        // The active branch's node must exist before asWidget mounts it.
        rebuild(
            at: view.activeIndex,
            view: view.branches[view.activeIndex],
            backend: backend,
            snapshot: snapshots?.count == view.branches.count
                ? snapshots?[view.activeIndex] : nil,
            environment: environment
        )
    }

    func node(at index: Int) -> ErasedViewGraphNode? {
        nodes.indices.contains(index) ? nodes[index] : nil
    }

    func mountedNode() -> ErasedViewGraphNode {
        node(at: mountedIndex) ?? nodes.compactMap { $0 }.first!
    }

    func snapshot(at index: Int) -> ViewGraphSnapshotter.NodeSnapshot? {
        slotSnapshots.indices.contains(index) ? slotSnapshots[index] : nil
    }

    func rebuild(
        at index: Int,
        view: AnyView,
        backend: some BaseAppBackend,
        snapshot: ViewGraphSnapshotter.NodeSnapshot?,
        environment: EnvironmentValues
    ) {
        nodes[index] = ErasedViewGraphNode(
            for: view,
            backend: backend,
            snapshot: snapshot,
            environment: environment
        )
        slotSnapshots[index] = nil
    }
}
