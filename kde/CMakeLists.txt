cmake_minimum_required(VERSION 3.22)
project(tuxedo-control-kde VERSION 0.3.0 LANGUAGES CXX)

find_package(ECM 6.0 REQUIRED NO_MODULE)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH})

include(KDEInstallDirs)
include(KDECMakeSettings)
include(KDECompilerSettings NO_POLICY_SCOPE)
include(ECMQmlModule)

find_package(Qt6 6.5 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2 Network DBus Widgets)
find_package(KF6 6.8 REQUIRED COMPONENTS Kirigami CoreAddons I18n)

qt_policy(SET QTP0001 NEW)

add_executable(tuxedo-control-kde src/main.cpp src/backend.cpp src/backend.h)

ecm_add_qml_module(tuxedo-control-kde URI org.andreasincode.tuxedocontrol GENERATE_PLUGIN_SOURCE)
ecm_target_qml_sources(tuxedo-control-kde SOURCES qml/Main.qml qml/CurveEditor.qml)

target_link_libraries(tuxedo-control-kde PRIVATE
    Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2 Qt6::Network Qt6::DBus Qt6::Widgets
    KF6::CoreAddons KF6::I18n KF6::I18nQml)
target_include_directories(tuxedo-control-kde PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/src)

install(TARGETS tuxedo-control-kde ${KDE_INSTALL_TARGETS_DEFAULT_ARGS})
install(FILES org.andreasincode.tuxedocontrol.desktop DESTINATION ${KDE_INSTALL_APPDIR})
