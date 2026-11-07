//! Turns what `validator` found into an error a person can be shown.
//!
//! The `Display` of [`validator::ValidationErrors`] prints the rule, its
//! parameters and the value that was sent. None of that goes into an answer.

use revolt_result::{failed_validation, Error, ValidationProblem};
use validator::{ValidationError, ValidationErrors, ValidationErrorsKind};

fn problem(field: String, error: &ValidationError) -> ValidationProblem {
    // the length rule sends whole numbers, the range rule floats
    let number = |name: &str| {
        let value = error.params.get(name)?;
        value.as_i64().or_else(|| value.as_f64().map(|number| number as i64))
    };
    let (min, max) = match number("equal") {
        Some(equal) => (Some(equal), Some(equal)),
        None => (number("min"), number("max")),
    };
    let rule = match error.code.as_ref() {
        "length" => "length",
        "range" => "range",
        "required" => "required",
        "email" | "url" | "regex" | "contains" => "format",
        _ => "invalid",
    };
    ValidationProblem {
        min,
        max,
        ..ValidationProblem::new(field, rule)
    }
}

fn collect(errors: &ValidationErrors, path: &str, out: &mut Vec<ValidationProblem>) {
    for (field, kind) in errors.errors() {
        let path = match path {
            "" => field.to_string(),
            parent => format!("{parent}.{field}"),
        };
        match kind {
            ValidationErrorsKind::Field(list) => {
                out.extend(list.iter().map(|error| problem(path.clone(), error)));
            }
            ValidationErrorsKind::Struct(inner) => collect(inner, &path, out),
            ValidationErrorsKind::List(items) => {
                for (index, inner) in items {
                    collect(inner, &format!("{path}[{index}]"), out);
                }
            }
        }
    }
}

/// `FailedValidation` for what `validate()` found. The full text goes to the
/// debug log only, because it holds what the person typed.
#[track_caller]
pub fn validation_failed(errors: &ValidationErrors) -> Error {
    log::debug!("validation failed: {errors}");
    let mut fields = Vec::new();
    collect(errors, "", &mut fields);
    fields.sort_by(|a, b| a.field.cmp(&b.field));
    failed_validation(fields)
}

#[cfg(test)]
mod tests {
    use revolt_result::ErrorType;
    use validator::Validate;

    use super::*;

    #[derive(Validate)]
    struct Inner {
        #[validate(length(max = 4))]
        title: String,
    }

    #[derive(Validate)]
    struct Form {
        #[validate(length(min = 20, max = 1000))]
        statement: String,
        #[validate(range(min = 1, max = 100))]
        limit: i64,
        #[validate]
        inner: Vec<Inner>,
    }

    fn error_of(form: &Form) -> (String, Vec<ValidationProblem>) {
        let error = validation_failed(&form.validate().unwrap_err());
        match error.error_type {
            ErrorType::FailedValidation { error, fields } => (error, fields),
            other => panic!("not a validation error: {other:?}"),
        }
    }

    #[test]
    fn the_answer_holds_no_validator_text_and_no_value() {
        let (error, fields) = error_of(&Form {
            statement: "typed by a person".to_string(),
            limit: 500,
            inner: vec![Inner { title: "too long".to_string() }],
        });
        for raw in ["Validation error", "[{", "\"value\"", "typed by a person"] {
            assert!(!error.contains(raw), "{raw} in {error}");
        }
        assert_eq!(
            error,
            "inner[0].title must be at most 4 characters; limit must be between 1 and 100; \
             statement must be between 20 and 1000 characters"
        );
        assert_eq!(fields[2], ValidationProblem::length("statement", 20, 1000));
        assert_eq!(fields[1].rule, "range");
    }
}
