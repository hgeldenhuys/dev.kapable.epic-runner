use crate::api_client::{ApiClient, DataWrapper};
use crate::types::{Impediment, ImpedimentStatus};

/// Check for blocking impediments via API (client-side filter).
pub async fn check_blocking_impediments(
    client: &ApiClient,
    epic_code: &str,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let resp: DataWrapper<Vec<serde_json::Value>> = client.get("/v1/impediments").await?;
    let blocking = resp
        .data
        .into_iter()
        .filter(|i| {
            i["blocking_epic"].as_str() == Some(epic_code) && i["status"].as_str() == Some("open")
        })
        .collect();
    Ok(blocking)
}

/// Check if an epic has unresolved impediments.
pub fn has_blocking_impediments(impediments: &[Impediment], epic_code: &str) -> bool {
    impediments.iter().any(|i| {
        i.blocking_epic == epic_code
            && (i.status == ImpedimentStatus::Open || i.status == ImpedimentStatus::Acknowledged)
    })
}

/// Get open impediments for a specific epic.
pub fn open_impediments_for_epic<'a>(
    impediments: &'a [Impediment],
    epic_code: &str,
) -> Vec<&'a Impediment> {
    impediments
        .iter()
        .filter(|i| {
            i.blocking_epic == epic_code
                && (i.status == ImpedimentStatus::Open
                    || i.status == ImpedimentStatus::Acknowledged)
        })
        .collect()
}

/// Format impediment for display.
pub fn format_impediment(imp: &Impediment) -> String {
    let blocked_by = imp.blocked_by_epic.as_deref().unwrap_or("external");
    let status_str = match imp.status {
        ImpedimentStatus::Open => "open",
        ImpedimentStatus::Acknowledged => "acknowledged",
        ImpedimentStatus::Resolved => "resolved",
        ImpedimentStatus::WontFix => "wont_fix",
    };
    format!(
        "[{}] {} → blocked by {} ({})",
        imp.blocking_epic, imp.title, blocked_by, status_str
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn make_impediment(blocking: &str, status: ImpedimentStatus) -> Impediment {
        Impediment {
            id: Uuid::new_v4(),
            product_id: Uuid::new_v4(),
            blocking_epic: blocking.to_string(),
            blocked_by_epic: Some("OTHER-001".to_string()),
            title: "Test impediment".to_string(),
            description: None,
            status,
            raised_by_sprint: None,
            resolved_by_sprint: None,
            created_at: Utc::now(),
            resolved_at: None,
        }
    }

    #[test]
    fn detects_blocking_impediments() {
        let imps = vec![
            make_impediment("AUTH-001", ImpedimentStatus::Open),
            make_impediment("AUTH-001", ImpedimentStatus::Resolved),
        ];
        assert!(has_blocking_impediments(&imps, "AUTH-001"));
    }

    #[test]
    fn resolved_impediments_dont_block() {
        let imps = vec![make_impediment("AUTH-001", ImpedimentStatus::Resolved)];
        assert!(!has_blocking_impediments(&imps, "AUTH-001"));
    }

    #[test]
    fn wrong_epic_doesnt_block() {
        let imps = vec![make_impediment("DATA-001", ImpedimentStatus::Open)];
        assert!(!has_blocking_impediments(&imps, "AUTH-001"));
    }
}
