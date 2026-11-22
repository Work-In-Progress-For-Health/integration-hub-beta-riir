-- The Clinical_Results database for the LIMS -> TRRR flow (`db-sender` with
-- DB_PROCEDURE_TYPE=trrr, spec/10-senders.md section 7.10).
--
-- From upstream `mock_receivers/db_mock_receiver/sql/init.sql` (#458). Azure SQL Edge ships no
-- `sqlcmd` (spec/18-deployment.md 5.3.5), so nothing runs this on start-up: apply it by hand or
-- from a test, as `init-drs-db.sql`. Idempotent. Parameter lists must match `crates/db-sender/src/trrr.rs`.

-- =============================================================================================
-- Mock TRRR "Clinical_Results" database for local testing of senders/db_sender_trrr
-- (lims-to-trrr flow). Independent of DRS above.
-- =============================================================================================
USE master;
GO

IF NOT EXISTS (SELECT name FROM sys.databases WHERE name = 'Clinical_Results')
BEGIN
    CREATE DATABASE Clinical_Results;
END;
GO

USE [Clinical_Results];
GO

-- Receives the LIMS -> NHS identifier associations written by db_sender_trrr.
IF OBJECT_ID('dbo.TRRRIdentifierUpdate', 'U') IS NULL
BEGIN
    CREATE TABLE dbo.TRRRIdentifierUpdate (
        Id                             INT IDENTITY(1,1) NOT NULL CONSTRAINT PK_TRRRIdentifierUpdate PRIMARY KEY CLUSTERED,
        LIMSIdentifier                 VARCHAR(100)      NOT NULL,
        NewIdentifier                  VARCHAR(100)      NOT NULL,
        NewIdentifierAssigningAuthority VARCHAR(50)      NOT NULL,
        ReceivedUtc                    DATETIME2(7)      NOT NULL CONSTRAINT DF_TRRRIdentifierUpdate_ReceivedUtc DEFAULT (SYSUTCDATETIME())
    );
END;
GO

-- Signature must match db_sender_trrr's db_client.execute_identifier_update:
-- (LIMSIdentifier, NewIdentifier, NewIdentifierAssigningAuthority). All arguments are text and are
-- stored exactly as received (leading zeroes retained).
CREATE OR ALTER PROCEDURE dbo.TRRRIDUpdate
    @LIMSIdentifier                  VARCHAR(100),
    @NewIdentifier                   VARCHAR(100),
    @NewIdentifierAssigningAuthority VARCHAR(50)
AS
BEGIN
    SET NOCOUNT ON;

    BEGIN TRY
        INSERT INTO dbo.TRRRIdentifierUpdate (LIMSIdentifier, NewIdentifier, NewIdentifierAssigningAuthority)
        VALUES (@LIMSIdentifier, @NewIdentifier, @NewIdentifierAssigningAuthority);
    END TRY
    BEGIN CATCH
        THROW;
    END CATCH;
END
GO

-- Receives the patient merge (superseded -> replacement identifier) requests written by db_sender_trrr
-- for ADT^A40 messages.
IF OBJECT_ID('dbo.TRRRIdentifierMerge', 'U') IS NULL
BEGIN
    CREATE TABLE dbo.TRRRIdentifierMerge (
        Id                              INT IDENTITY(1,1) NOT NULL CONSTRAINT PK_TRRRIdentifierMerge PRIMARY KEY CLUSTERED,
        OldIdentifier                   VARCHAR(100)      NOT NULL,
        OldIdentifierAssigningAuthority VARCHAR(50)       NOT NULL,
        NewIdentifier                   VARCHAR(100)      NOT NULL,
        NewIdentifierAssigningAuthority VARCHAR(50)       NOT NULL,
        ReceivedUtc                     DATETIME2(7)      NOT NULL CONSTRAINT DF_TRRRIdentifierMerge_ReceivedUtc DEFAULT (SYSUTCDATETIME())
    );
END;
GO

-- Signature must match db_sender_trrr's db_client.execute_identifier_update for merge requests:
-- (OldIdentifier, OldIdentifierAssigningAuthority, NewIdentifier, NewIdentifierAssigningAuthority).
-- All arguments are text and are stored exactly as received (leading zeroes retained).
CREATE OR ALTER PROCEDURE dbo.TRRRIDMerge
    @OldIdentifier                   VARCHAR(100),
    @OldIdentifierAssigningAuthority VARCHAR(50),
    @NewIdentifier                   VARCHAR(100),
    @NewIdentifierAssigningAuthority VARCHAR(50)
AS
BEGIN
    SET NOCOUNT ON;

    BEGIN TRY
        INSERT INTO dbo.TRRRIdentifierMerge
            (OldIdentifier, OldIdentifierAssigningAuthority, NewIdentifier, NewIdentifierAssigningAuthority)
        VALUES
            (@OldIdentifier, @OldIdentifierAssigningAuthority, @NewIdentifier, @NewIdentifierAssigningAuthority);
    END TRY
    BEGIN CATCH
        THROW;
    END CATCH;
END
GO
