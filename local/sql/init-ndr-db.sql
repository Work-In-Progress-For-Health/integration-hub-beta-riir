-- The NDR database for the CAV PMS -> WCRS flow (`db-sender` with
-- DB_PROCEDURE_TYPE=update_identifier, spec/10-senders.md section 7.9).
--
-- From upstream `mock_receivers/db_mock_receiver/sql/init.sql` (#463). The real
-- procedure's definition is not available, so this mock only stores its three inputs;
-- revisit once the real signature is known. Azure SQL Edge ships no `sqlcmd`
-- (spec/18-deployment.md 5.3.5), so nothing runs this on start-up: apply it by hand or
-- from a test, as `init-drs-db.sql`. Idempotent. Parameter order must match
-- `crates/db-sender/src/idupdate.rs`.
USE master;
GO

IF NOT EXISTS (SELECT name FROM sys.databases WHERE name = 'NDR')
BEGIN
    CREATE DATABASE NDR;
END;
GO

USE [NDR];
GO

-- Receives the identifier updates written via dbo.spUpdateIdentifier, so testers can see
-- what was received: SELECT * FROM NDR.dbo.IdentifierUpdate.
IF OBJECT_ID('dbo.IdentifierUpdate', 'U') IS NULL
BEGIN
    CREATE TABLE dbo.IdentifierUpdate (
        Id                              INT IDENTITY(1,1) NOT NULL CONSTRAINT PK_IdentifierUpdate PRIMARY KEY CLUSTERED,
        OldIdentifier                   VARCHAR(100)      NOT NULL,
        OldIdentifierAssigningAuthority VARCHAR(50)       NOT NULL,
        NewIdentifier                   VARCHAR(100)      NOT NULL,
        ReceivedUtc                     DATETIME2(7)      NOT NULL CONSTRAINT DF_IdentifierUpdate_ReceivedUtc DEFAULT (SYSUTCDATETIME())
    );
END;
GO

-- Arguments are text and are stored exactly as received (leading zeroes retained).
CREATE OR ALTER PROCEDURE dbo.spUpdateIdentifier
    @OldIdentifier                   VARCHAR(100),
    @OldIdentifierAssigningAuthority VARCHAR(50),
    @NewIdentifier                   VARCHAR(100)
AS
BEGIN
    SET NOCOUNT ON;

    BEGIN TRY
        INSERT INTO dbo.IdentifierUpdate (OldIdentifier, OldIdentifierAssigningAuthority, NewIdentifier)
        VALUES (@OldIdentifier, @OldIdentifierAssigningAuthority, @NewIdentifier);
    END TRY
    BEGIN CATCH
        THROW;
    END CATCH;
END
GO
