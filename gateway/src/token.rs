use serde::Deserialize;
use worker::js_sys;
use worker::D1Database;

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct TokenRow {
    pub token: String,
    pub provider: String,
    pub provisioned_cost: f64,
    pub current_cost: f64,
    pub creation_time: f64,
    pub expiration_time: f64,
    pub allowed_ips: String,
}

/// Looks up a token and, in the same query, enforces validity: `None` means the token doesn't
/// exist, is expired, is over budget (`current_cost >= provisioned_cost`), or doesn't match
/// `provider` — callers can't tell these cases apart from the result.
pub async fn get_token(
    db: &D1Database,
    token: &str,
    provider: &str,
) -> worker::Result<Option<TokenRow>> {
    let now = js_sys::Date::now();
    db.prepare("SELECT * FROM tokens WHERE token = ?1 AND expiration_time > ?2 AND current_cost < provisioned_cost AND provider = ?3")
        .bind(&[token.into(), now.into(), provider.into()])?
        .first::<TokenRow>(None)
        .await
}

pub async fn update_cost(
    db: &D1Database,
    token: &str,
    cost: f64,
    provider: &str,
) -> worker::Result<()> {
    // Authorization happens before the upstream call, so this increment is intentionally not
    // guarded by the budget predicate; adding one here would only reject the charge after the
    // provider has already been paid and the response has already been delivered.
    db.prepare(
        "UPDATE tokens SET current_cost = current_cost + ?1 WHERE token = ?2 AND provider = ?3",
    )
    .bind(&[cost.into(), token.into(), provider.into()])?
    .run()
    .await?;
    Ok(())
}
