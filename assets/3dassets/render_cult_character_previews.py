"""Render the four procedurally generated Cult campaign character variants."""

from pathlib import Path
import sys

import bpy
from mathutils import Vector

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from build_first_char_rig import apply_department_palette, set_variant_visibility


def point_at(obj, target):
    obj.rotation_euler = (Vector(target) - obj.location).to_track_quat("-Z", "Y").to_euler()


def main():
    scene = bpy.context.scene
    scene.render.engine = "BLENDER_EEVEE"
    scene.render.resolution_x = 520
    scene.render.resolution_y = 720
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.world.color = (0.018, 0.022, 0.030)

    bpy.ops.object.camera_add(location=(0.92, -2.80, 0.02))
    camera = bpy.context.object
    camera.data.lens = 58
    point_at(camera, (0.0, 0.0, -0.07))
    scene.camera = camera

    for location, energy, size in [((-1.8, -2.2, 2.4), 850, 2.2), ((1.8, -0.8, 1.1), 500, 1.5)]:
        bpy.ops.object.light_add(type="AREA", location=location)
        light = bpy.context.object
        light.data.energy = energy
        light.data.shape = "DISK"
        light.data.size = size
        point_at(light, (0.0, 0.0, -0.1))

    bpy.ops.mesh.primitive_plane_add(size=5, location=(0.0, 0.0, -0.95))
    floor = bpy.context.object
    floor_material = bpy.data.materials.new("CultPreviewFloor")
    floor_material.diffuse_color = (0.040, 0.050, 0.063, 1.0)
    floor.data.materials.append(floor_material)

    output = HERE / "previews" / "cult_characters"
    output.mkdir(parents=True, exist_ok=True)
    rig = bpy.data.objects["CharacterRig"]
    rig.animation_data.action = bpy.data.actions["Idle"]
    scene.frame_set(25)
    details = [(obj, None) for obj in bpy.data.objects if obj.get("chemgame_character_detail")]
    views = (
        ("", (0.92, -2.80, 0.02)),
        ("_profile", (2.82, -0.30, 0.02)),
        ("_rear", (-0.72, 2.86, 0.02)),
    )
    for variant in ("corwin", "cult_watching", "cult_silent", "cult_blooded"):
        apply_department_palette(variant)
        set_variant_visibility(details, variant, face_variant=0)
        for suffix, location in views:
            camera.location = location
            point_at(camera, (0.0, 0.0, -0.07))
            scene.render.filepath = str(output / f"first_char_{variant}{suffix}.png")
            bpy.ops.render.render(write_still=True)
            print(f"RENDERED {scene.render.filepath}")

    set_variant_visibility(details, "player", face_variant=0)


if __name__ == "__main__":
    main()
