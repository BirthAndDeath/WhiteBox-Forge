use pevy::prelude::*;
pub fn ui(){
    App::new()
        .add_plugins(DefaultPlugins);
        .add_system(Startup,setup)
        .add_systems(Update, grow)
        .run();
}
fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn(Camera2d);
    // 加载图片
    let texture = asset_server.load("ico.png");

    // 生成精灵实体，挂上贴图
    commands.spawn((
        Sprite {
            image: texture,
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 0.0),
        GrowTimer(Timer::from_seconds(3.0, TimerMode::Once)),
    ));
}
fn grow(
    mut commands: Commands,
    time: Res<Time>,
    mut query: Query<(Entity, &mut Transform, &mut GrowTimer)>,
) {
    const MAX_SCALE: f32 = 3.0;

    for (entity, mut transform, mut timer) in &mut query {
        timer.0.tick(time.delta());

        if timer.0.finished() {
            transform.scale += Vec3::splat(0.5 * time.delta_secs());

            if transform.scale.x >= MAX_SCALE {
                commands.entity(entity).despawn();
            }
        }
    }
}