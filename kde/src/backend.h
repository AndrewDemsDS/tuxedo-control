// SPDX-License-Identifier: MIT
#pragma once

#include <QObject>
#include <QTimer>
#include <QVariantMap>
#include <qqmlregistration.h>

// Talks to tuxedo-controld over /run/tuxedo-control.sock (one request line, one
// response line) and to tlp-pd's power-profiles D-Bus API. No hardware access here.
class Backend : public QObject
{
    Q_OBJECT
    QML_ELEMENT
    QML_SINGLETON

    Q_PROPERTY(bool daemonUp READ daemonUp NOTIFY statusChanged)
    Q_PROPERTY(QVariantMap status READ status NOTIFY statusChanged)
    Q_PROPERTY(QVariantMap profiles READ profiles NOTIFY profilesChanged)
    Q_PROPERTY(QString powerProfile READ powerProfile NOTIFY powerProfileChanged)
    Q_PROPERTY(QStringList powerProfiles READ powerProfiles NOTIFY powerProfileChanged)

public:
    explicit Backend(QObject *parent = nullptr);

    bool daemonUp() const { return m_up; }
    QVariantMap status() const { return m_status; }
    QVariantMap profiles() const { return m_profiles; }
    QString powerProfile() const { return m_power; }
    QStringList powerProfiles() const { return m_powers; }

    // Raw command; returns the response line, or "" if the daemon is unreachable.
    Q_INVOKABLE QString send(const QString &cmd);
    Q_INVOKABLE bool saveProfile(const QString &name, const QString &perf, const QVariantList &curve,
                                 const QVariant &kbd, const QVariant &charge);
    Q_INVOKABLE bool setPowerProfile(const QString &profile);
    Q_INVOKABLE int safetyFloor(int temp) const;
    Q_INVOKABLE void refresh();

Q_SIGNALS:
    void statusChanged();
    void profilesChanged();
    void powerProfileChanged();

private Q_SLOTS:
    void onPowerPropertiesChanged(const QString &iface, const QVariantMap &changed, const QStringList &invalidated);

private:
    void readPowerProfile();

    QTimer m_timer;
    bool m_up = false;
    QVariantMap m_status;
    QVariantMap m_profiles;
    QByteArray m_profilesRaw;
    QString m_power;
    QStringList m_powers;
};
