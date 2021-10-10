use bme680::{
    Bme680, FieldDataCondition, I2CAddress, IIRFilterSize, OversamplingSetting, PowerMode,
    SettingsBuilder,
};
use dotenv::var;
use futures::stream;
use influxdb2_client::models::DataPoint;
use linux_embedded_hal::*;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), ()> {
    tracing_subscriber::fmt::init();
    dotenv::dotenv().map_err(|e| error!("Failed to load .env file for configuration: {:?}", e))?;
    let influx_token =
        var("INFLUX_TOKEN").map_err(|e| error!("Failed to load INFLUX_TOKEN: {:?}", e))?;
    let influx_bucket =
        var("INFLUX_BUCKET").map_err(|e| error!("Failed to load INFLUX_BUCKET: {:?}", e))?;
    let influx_address =
        var("INFLUX_ADDRESS").map_err(|e| error!("Failed to load INFLUX_BUCKET: {:?}", e))?;
    let influx_organization = var("INFLUX_ORGANIZATION")
        .map_err(|e| error!("Failed to load INFLUX_ORGANIZATION: {:?}", e))?;

    let host_tag = var("HOSTNAME").unwrap_or_else(|_| "unknown".into());

    let temperature_offset: f32 = var("TEMP_OFFSET")
        .unwrap_or_else(|_| "0".into())
        .parse()
        .map_err(|e| error!("Failed to load temp offset from TEMP_OFFSET: {:?}", e))?;

    let i2c =
        I2cdev::new("/dev/i2c-1").map_err(|e| error!("Failed to load I2C device: {:?}", e))?;
    let mut delayer = Delay {};
    let mut dev = Bme680::init(i2c, &mut delayer, I2CAddress::Secondary)
        .map_err(|e| error!("Failed to init BME680. {:?}", e))?;

    let settings = SettingsBuilder::new()
        .with_humidity_oversampling(OversamplingSetting::OS2x)
        .with_pressure_oversampling(OversamplingSetting::OS4x)
        .with_temperature_oversampling(OversamplingSetting::OS8x)
        .with_temperature_filter(IIRFilterSize::Size3)
        .with_gas_measurement(Duration::from_millis(1500), 320, 25)
        .with_run_gas(true)
        .with_temperature_offset(temperature_offset)
        .build();
    dev.set_sensor_settings(&mut delayer, settings)
        .map_err(|e| error!("Failed to set BME680 sensor settings. {:?}", e))?;
    let mut profile_dur = dev
        .get_profile_dur(&settings.0)
        .map_err(|e| error!("Failed to get profile duration: {:?}", e))?;

    info!("Profile duration set to: {:?}", &profile_dur);
    profile_dur *=3;
    info!("Tripling duration to: {:?}", profile_dur);

    let client = influxdb2_client::Client::new(influx_address, influx_token);

    info!("Waiting 5m for device to stabilize before reading.");
    sleep(Duration::from_secs(5*60)).await;
    info!("Starting readings.");

    loop {
        dev.set_sensor_mode(&mut delayer, PowerMode::ForcedMode)
            .map_err(|e| error!("Failed to set PowerMode::ForcedMode {:?}", e))?;
        let (data, state) = dev
            .get_sensor_data(&mut delayer)
            .map_err(|e| error!("Failed to get sensor reading {:?}", e))?;

        if state == FieldDataCondition::NewData {
            let points = vec![
                DataPoint::builder("temperature_c")
                    .tag("host", &host_tag)
                    .field("value", data.temperature_celsius() as f64)
                    .build()
                    .map_err(|e| error!("Failed to create data point temperature_c {:?}", e))?,
                DataPoint::builder("relative_humidity")
                    .tag("host", &host_tag)
                    .field("value", data.humidity_percent() as f64)
                    .build()
                    .map_err(|e| error!("Failed to create data point relative_humidity {:?}", e))?,
                DataPoint::builder("pressure_hpa")
                    .tag("host", &host_tag)
                    .field("value", data.pressure_hpa() as f64)
                    .build()
                    .map_err(|e| error!("Failed to create data point pressure_hpa {:?}", e))?,
                DataPoint::builder("gas_resistance_ohms")
                    .tag("host", &host_tag)
                    .field("value", data.gas_resistance_ohm() as f64)
                    .build()
                    .map_err(|e| {
                        error!("Failed to create data point gas_resistance_ohms {:?}", e)
                    })?,
            ];

            match client
                .write(&influx_organization, &influx_bucket, stream::iter(points))
                .await {
                Ok(_) => (),
                Err(e) => error!("Failed to write data points to influxdb: {:?}", e)
            };
        }
        sleep(profile_dur).await;
    }
}
