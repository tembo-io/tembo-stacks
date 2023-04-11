use serde_json::{json, Value};

use crate::aws::cloudformation::CloudFormationParams;

pub async fn create_template(params: &CloudFormationParams) -> Value {
    let bucket_policy = s3_bucket_policy(&params.s3_bucket_name, &params.s3_bucket_path).await;
    let bucket_iam_role = iam_role_template(
        &params.iam_role_name,
        &params.s3_bucket_name,
        &params.s3_bucket_path,
    )
    .await;
    let bucket_lifecycle = s3_bucket_lifecycle_policy_template(
        &params.s3_bucket_name,
        &params.s3_bucket_path,
        params.lifecycle_duration,
    )
    .await;

    let cf_resources = json!({
        "S3BucketPolicy": bucket_policy,
        "IAMRole": bucket_iam_role,
        "LifecycleConfiguration": bucket_lifecycle,
    });

    let cf_template = json!({
        "Parameters": {
            "BucketName": {
                "Type": "String",
                "Description": "Name of the S3 bucket",
            },
            "BucketPath": {
                "Type": "String",
                "Description": "Path to the S3 bucket",
            },
            "RoleName": {
                "Type": "String",
                "Description": "Name for the IAM role",
            },
            "ExpirationInDays": {
                "Type": "Number",
                "Description": "Number of days after which objects in the specified path will expire",
                "Default": bucket_lifecycle,
            },
        },
        "Resources": cf_resources,
    });

    cf_template
}

async fn s3_bucket_policy(bucket_name: &str, bucket_path: &str) -> Value {
    json!({
        "Type": "AWS::S3::BucketPolicy",
        "Properties": {
            "Bucket": bucket_name,
            "PolicyDocument": {
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": ["s3:GetObject"],
                        "Resource": [
                            format!("arn:aws:s3:::{}/{}/*", bucket_name, bucket_path)
                        ],
                        "Principal": {
                            "AWS": {
                                "Fn::GetAtt": [
                                    "IAMRole.Arn",
                                    "Arn"
                                ]
                            }
                        }
                    },
                    {
                        "Effect": "Allow",
                        "Action": [
                            "s3:PutObject",
                            "s3:GetObject",
                            "s3:ListBucket",
                            "s3:ReplicateObject",
                            "s3:RestoreObject",
                            "s3:PutLifecycleConfiguration"
                        ],
                        "Resource": [
                            format!("arn:aws:s3:::{}/{}*", bucket_name, bucket_path),
                            format!("arn:aws:s3:::{}/{}/*", bucket_name, bucket_path)
                        ],
                        "Principal": {
                            "AWS": {
                                "Fn::GetAtt": [
                                    "IAMRole.Arn",
                                    "Arn"
                                ]
                            }
                        }
                    }
                ]
            }
        }
    })
}

async fn iam_role_template(iam_role_name: &str, bucket_name: &str, bucket_path: &str) -> Value {
    json!({
        "Type": "AWS::IAM::Role",
        "Properties": {
            "RoleName": iam_role_name,
            "AssumeRolePolicyDocument": {
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Principal": { "Service": "s3.amazonaws.com" },
                        "Action": "sts:AssumeRole"
                    }
                ]
            },
            "Path": "/",
            "Policies": [
                {
                    "PolicyName": "s3-access",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [
                            {
                                "Effect": "Allow",
                                "Action": [
                                    "s3:GetObject",
                                    "s3:PutObject",
                                    "s3:ListBucket",
                                    "s3:ReplicateObject",
                                    "s3:RestoreObject",
                                    "s3:PutLifecycleConfiguration"
                                ],
                                "Resource": [
                                    format!("arn:aws:s3:::{}/{}*", bucket_name, bucket_path),
                                    format!("arn:aws:s3:::{}/{}/*", bucket_name, bucket_path)
                                ]
                            }
                        ]
                    }
                }
            ]
        }
    })
}

async fn s3_bucket_lifecycle_policy_template(
    bucket_name: &str,
    bucket_path: &str,
    expiration_in_days: Option<u32>,
) -> Value {
    json!({
    "Type": "AWS::S3::BucketLifecycleConfiguration",
        "Properties": {
            "Bucket": bucket_name,
            "LifecycleConfiguration": {
                "Rules": [
                    {
                        "Status": "Enabled",
                        "Prefix": bucket_path,
                        "ExpirationInDays": expiration_in_days,
                    }
                ]
            }
        }
    })
}
