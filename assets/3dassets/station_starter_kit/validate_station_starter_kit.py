"""Validate the generated ChemGame station starter kit.

Run after generation with Blender's bundled Python:

    blender --background station_starter_kit.blend --python validate_station_starter_kit.py

The checks cover the editable source, the public manifest, and every exported
GLB. Nothing is saved or modified on disk.
"""

from __future__ import annotations

import json
import math
import struct
import traceback
from pathlib import Path

import bpy
from mathutils import Vector


HERE = Path(__file__).resolve().parent
MANIFEST_PATH = HERE / "station_starter_kit_manifest.json"
GLB_DIR = HERE / "glb"
EPSILON = 2.0e-4
DEPARTMENT_SHELL_EXTRA = "cg_department_shell"

ERRORS: list[str] = []


def require(condition: bool, message: str) -> None:
    if not condition:
        ERRORS.append(message)


def close_enough(actual: list[float] | tuple[float, ...], expected: list[float]) -> bool:
    return len(actual) == len(expected) and all(
        math.isclose(left, right, rel_tol=0.0, abs_tol=EPSILON)
        for left, right in zip(actual, expected)
    )


def read_glb_json(path: Path) -> dict:
    raw = path.read_bytes()
    require(len(raw) >= 20, f"{path.name}: truncated GLB header")
    if len(raw) < 20:
        return {}

    magic, version, declared_length = struct.unpack_from("<4sII", raw, 0)
    require(magic == b"glTF", f"{path.name}: bad GLB magic")
    require(version == 2, f"{path.name}: expected glTF 2, got {version}")
    require(
        declared_length == len(raw),
        f"{path.name}: header length {declared_length} != file length {len(raw)}",
    )
    json_length, json_type = struct.unpack_from("<II", raw, 12)
    require(json_type == 0x4E4F534A, f"{path.name}: first chunk is not JSON")
    return json.loads(raw[20 : 20 + json_length].decode("utf-8"))


def validate_bevy_scene_contract(document: dict, path: Path) -> None:
    """Reject Blender scene JSON that Bevy 0.19's stricter parser cannot read."""

    scenes = document.get("scenes", [])
    require(len(scenes) == 1, f"{path.name}: expected exactly one exported scene")
    for index, scene in enumerate(scenes):
        require(
            "nodes" in scene,
            f"{path.name}: scene {index} omits nodes and is not Bevy-compatible",
        )


def measured_import_bounds(path: Path, asset_id: str) -> tuple[list[float], list[float]]:
    before = {obj.as_pointer() for obj in bpy.data.objects}
    bpy.ops.import_scene.gltf(filepath=str(path))
    imported = [obj for obj in bpy.data.objects if obj.as_pointer() not in before]
    roots = [
        obj
        for obj in imported
        if obj.type == "EMPTY" and obj.get("asset_id") == asset_id
    ]
    require(len(roots) == 1, f"{path.name}: imported {len(roots)} semantic roots")
    if len(roots) != 1:
        for obj in imported:
            bpy.data.objects.remove(obj, do_unlink=True)
        return [0.0, 0.0, 0.0], [0.0, 0.0, 0.0]

    bpy.context.view_layer.update()
    root = roots[0]
    root_inverse = root.matrix_world.inverted()
    depsgraph = bpy.context.evaluated_depsgraph_get()
    points: list[Vector] = []
    for obj in imported:
        if obj.type != "MESH":
            continue
        evaluated = obj.evaluated_get(depsgraph)
        evaluated_mesh = evaluated.to_mesh()
        try:
            object_to_root = root_inverse @ evaluated.matrix_world
            points.extend(object_to_root @ vertex.co for vertex in evaluated_mesh.vertices)

            # Audit the exported triangles, not only the editable source boxes.
            # Oversized bevels on thin parts can otherwise fold into zero-area
            # slivers or emit vertex normals opposite their winding.
            evaluated_mesh.calc_loop_triangles()
            corner_normals = evaluated_mesh.corner_normals
            degenerate = 0
            opposed = 0
            for triangle in evaluated_mesh.loop_triangles:
                if triangle.area <= 1.0e-12:
                    degenerate += 1
                    continue
                averaged = Vector((0.0, 0.0, 0.0))
                for loop_index in triangle.loops:
                    averaged += corner_normals[loop_index].vector
                if averaged.length_squared > 1.0e-12:
                    averaged.normalize()
                    if triangle.normal.dot(averaged) < -0.5:
                        opposed += 1
            require(
                degenerate == 0,
                f"{path.name}: {obj.name} has {degenerate} degenerate exported triangles",
            )
            require(
                opposed == 0,
                f"{path.name}: {obj.name} has {opposed} triangles opposed to vertex normals",
            )
        finally:
            evaluated.to_mesh_clear()

    require(bool(points), f"{path.name}: imported no mesh vertices")
    minimum = [min(point[axis] for point in points) for axis in range(3)]
    maximum = [max(point[axis] for point in points) for axis in range(3)]

    for obj in imported:
        bpy.data.objects.remove(obj, do_unlink=True)
    return minimum, maximum


def validate_source(manifest: dict) -> None:
    asset_collections = [
        collection
        for collection in bpy.data.collections
        if collection.name.startswith("ASSET_") and collection.get("cg_generated")
    ]
    require(
        len(asset_collections) == len(manifest["assets"]),
        f"editable source has {len(asset_collections)} asset collections; manifest has {len(manifest['assets'])}",
    )

    socket_node_names: list[str] = []
    tagged_boxes = 0
    for asset in manifest["assets"]:
        asset_id = asset["asset_id"]
        root = bpy.data.objects.get(f"ROOT_{asset_id}")
        require(root is not None, f"{asset_id}: missing editable root")
        if root is None:
            continue
        require(root.get("source_front_axis") == "-Y", f"{asset_id}: bad source front")
        require(root.get("export_front_axis") == "+Z", f"{asset_id}: bad export front")

        for socket_name, socket in asset["sockets"].items():
            node_name = socket["node"]
            socket_node_names.append(node_name)
            node = bpy.data.objects.get(node_name)
            require(node is not None, f"{asset_id}: missing socket node {node_name}")
            if node is not None:
                require(
                    node.get("cg_socket") == socket_name,
                    f"{asset_id}: {node_name} has wrong cg_socket extra",
                )
                require(
                    close_enough(tuple(node.location), socket["blender_xyz_m"]),
                    f"{asset_id}: {node_name} source position differs from manifest",
                )

    require(
        len(socket_node_names) == len(set(socket_node_names)),
        "socket node names are not globally unique",
    )

    for obj in bpy.data.objects:
        if obj.get("cg_primitive") != "box" or obj.type != "MESH":
            continue
        tagged_boxes += 1
        for polygon in obj.data.polygons:
            require(
                polygon.center.dot(polygon.normal) > 0.0,
                f"{obj.name}: box face {polygon.index} points inward",
            )
    require(tagged_boxes > 100, f"only {tagged_boxes} generated boxes were tagged")

    for material in bpy.data.materials:
        if material.name.startswith("MAT_") and material.diffuse_color[3] >= 0.999:
            require(
                material.use_backface_culling,
                f"{material.name}: opaque material is not back-face culled",
            )


def validate_exports(manifest: dict) -> None:
    expected_files = {Path(asset["file"]).name for asset in manifest["assets"]}
    expected_files.update(
        Path(asset["runtime_dressing_file"]).name
        for asset in manifest["assets"]
        if asset.get("runtime_dressing_file")
    )
    actual_files = {path.name for path in GLB_DIR.glob("*.glb")}
    require(actual_files == expected_files, "GLB directory does not match the manifest")

    for asset in manifest["assets"]:
        path = HERE / asset["file"]
        require(path.is_file(), f"{asset['asset_id']}: missing {asset['file']}")
        if not path.is_file():
            continue

        document = read_glb_json(path)
        validate_bevy_scene_contract(document, path)
        nodes = document.get("nodes", [])
        nodes_by_name = {node.get("name"): node for node in nodes}
        require(
            f"ROOT_{asset['asset_id']}" in nodes_by_name,
            f"{path.name}: missing named root node",
        )

        for socket_name, socket in asset["sockets"].items():
            node = nodes_by_name.get(socket["node"])
            require(node is not None, f"{path.name}: missing {socket['node']}")
            if node is None:
                continue
            require(
                node.get("extras", {}).get("cg_socket") == socket_name,
                f"{path.name}: {socket['node']} lost its semantic extra",
            )
            require(
                close_enough(node.get("translation", [0.0, 0.0, 0.0]), socket["gltf_xyz_m"]),
                f"{path.name}: {socket['node']} export position differs from manifest",
            )

        for material in document.get("materials", []):
            require(
                not (
                    material.get("doubleSided") is True
                    and material.get("alphaMode", "OPAQUE") == "OPAQUE"
                ),
                f"{path.name}: opaque material {material.get('name', '<unnamed>')} is double-sided",
            )

        imported_minimum, imported_maximum = measured_import_bounds(
            path, asset["asset_id"]
        )
        source_bounds = asset["visual_bounds"]["blender"]
        require(
            close_enough(imported_minimum, source_bounds["min_xyz_m"]),
            f"{path.name}: imported minimum differs from measured source bounds",
        )
        require(
            close_enough(imported_maximum, source_bounds["max_xyz_m"]),
            f"{path.name}: imported maximum differs from measured source bounds",
        )

        runtime_file = asset.get("runtime_dressing_file")
        if runtime_file is None:
            require(
                asset["category"] != "department",
                f"{asset['asset_id']}: department has no runtime dressing export",
            )
            continue

        require(
            asset["category"] == "department",
            f"{asset['asset_id']}: non-department has a runtime dressing export",
        )
        runtime_path = HERE / runtime_file
        require(runtime_path.is_file(), f"{asset['asset_id']}: missing {runtime_file}")
        if not runtime_path.is_file():
            continue

        runtime_document = read_glb_json(runtime_path)
        validate_bevy_scene_contract(runtime_document, runtime_path)
        runtime_nodes = runtime_document.get("nodes", [])
        runtime_nodes_by_name = {node.get("name"): node for node in runtime_nodes}
        require(
            f"ROOT_{asset['asset_id']}" in runtime_nodes_by_name,
            f"{runtime_path.name}: missing named root node",
        )
        require(
            any("mesh" in node for node in runtime_nodes),
            f"{runtime_path.name}: contains no fixture geometry",
        )
        require(
            not any(
                node.get("extras", {}).get(DEPARTMENT_SHELL_EXTRA)
                for node in runtime_nodes
            ),
            f"{runtime_path.name}: still contains tagged map-shell geometry",
        )
        for socket_name, socket in asset["sockets"].items():
            node = runtime_nodes_by_name.get(socket["node"])
            require(
                node is not None,
                f"{runtime_path.name}: missing {socket['node']}",
            )
            if node is not None:
                require(
                    node.get("extras", {}).get("cg_socket") == socket_name,
                    f"{runtime_path.name}: {socket['node']} lost its semantic extra",
                )
        for material in runtime_document.get("materials", []):
            require(
                not (
                    material.get("doubleSided") is True
                    and material.get("alphaMode", "OPAQUE") == "OPAQUE"
                ),
                f"{runtime_path.name}: opaque material {material.get('name', '<unnamed>')} is double-sided",
            )
        measured_import_bounds(runtime_path, asset["asset_id"])


def main() -> None:
    require(bool(bpy.data.filepath), "open station_starter_kit.blend before validation")
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    require(manifest.get("generator_version") == 1, "unexpected generator version")
    require(len(manifest.get("assets", [])) == 14, "expected fourteen assets")
    require(
        manifest.get("coordinate_spaces", {})
        .get("export_gltf_bevy", {})
        .get("axes", {})
        .get("interaction_front")
        == "+Z",
        "manifest does not declare Bevy +Z as the interaction front",
    )

    validate_source(manifest)
    validate_exports(manifest)
    require(
        not (HERE / "station_starter_kit.blend1").exists(),
        "stale station_starter_kit.blend1 backup is present",
    )
    for preview in ["preview_machines.png", "preview_departments.png"]:
        path = HERE / preview
        require(path.is_file() and path.stat().st_size > 100_000, f"{preview} is missing or empty")

    if ERRORS:
        details = "\n - ".join(ERRORS)
        raise RuntimeError(f"station starter kit validation failed:\n - {details}")
    print(
        "Validated 14 editable assets, 20 GLBs, all node contracts, imported "
        "visual bounds, shell-free runtime dressing, source/exported normals, "
        "nondegenerate triangles, material culling, sockets, and previews."
    )


if __name__ == "__main__":
    try:
        main()
    except Exception:
        # Blender otherwise logs Python exceptions but still exits with status
        # zero, which makes a failed validation look green in a shell or CI.
        traceback.print_exc()
        raise SystemExit(1)
